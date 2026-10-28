# The Windows installer, end to end (needs an elevated PowerShell; CI runs it on windows-latest):
#   install silently -> service registered right -> configure -> start -> health -> MLLP round trip
#   -> log and Event Log written -> clean stop -> upgrade over a running, configured install
#   -> an upgrade leaves a service stopped on purpose stopped -> run as a named local account
#   (`ductus service account`) -> an upgrade keeps the account and waits for its password
#   -> uninstall -> install with SERVICEACCOUNT -> uninstall.
#
# Upgrades need a second build of the installer (-UpgradeMsi): each build has its own ProductCode,
# so one over the other is a real (major) upgrade. The same file again is only a maintenance run of
# the installed product: nothing is removed or reinstalled, and it proves nothing about upgrades.
#
#   .\scripts\demo-windows.ps1 -Msi dist\ductus-0.5.0-dev-x64.msi -UpgradeMsi dist2\ductus-0.5.0-dev-x64.msi -DatabaseUrl postgres://u:p@localhost/db
param(
    [Parameter(Mandatory)] [string] $Msi,
    [Parameter(Mandatory)] [string] $UpgradeMsi,
    [Parameter(Mandatory)] [string] $DatabaseUrl,
    # Where the installer logs and the receiver's output go (CI uploads it).
    [string] $LogDir = ""
)
$ErrorActionPreference = "Stop"
$Msi = (Resolve-Path $Msi).Path
$UpgradeMsi = (Resolve-Path $UpgradeMsi).Path
# Which build is installed: an upgrade installs the other one.
$installed = $Msi
$data = "C:\ProgramData\Ductus"
$exe = "C:\Program Files\Ductus\ductus.exe"
$work = if ($LogDir) { $LogDir } else { Join-Path ([IO.Path]::GetTempPath()) ("ductus-win-" + [guid]::NewGuid().ToString("N").Substring(0, 8)) }
New-Item -ItemType Directory -Force $work | Out-Null
$failures = @()
$sink = $null
$began = Get-Date
# A local account standing in for a gMSA (a gMSA needs a domain): it has a password, as a
# domain service account would.
$account = "ductus-svc-test"
# 14 characters: `net user` asks a question (Y/N) for longer ones.
$password = "Dx-" + [guid]::NewGuid().ToString("N").Substring(0, 8) + "!a1"

function Check([string] $name, [bool] $ok, [string] $detail = "") {
    if ($ok) { Write-Host "   ok: $name" } else { Write-Host "   FAILED: $name $detail"; $script:failures += $name }
}
function Msi([string[]] $arguments, [string] $log) {
    $p = Start-Process msiexec.exe -ArgumentList ($arguments + @("/qn", "/l*v", "`"$work\$log.log`"")) -Wait -PassThru
    Write-Host "   (msiexec $($arguments -join ' ') -> exit $($p.ExitCode); log $work\$log.log)"
    return $p.ExitCode
}
# Install the other build over the installed one, and check it really was an upgrade.
function Upgrade([string] $log, [string[]] $more = @()) {
    $next = if ($script:installed -eq $Msi) { $UpgradeMsi } else { $Msi }
    $ok = (Msi (@("/i", "`"$next`"") + $more) $log) -in 0, 3010
    $script:installed = $next
    $real = (Get-Content "$work\$log.log" -Raw) -match "Product: Ductus -- Installation completed successfully"
    return $ok -and $real
}
function Wait-Until([scriptblock] $condition, [int] $seconds = 60) {
    $until = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $until) { if (& $condition) { return $true }; Start-Sleep -Milliseconds 500 }
    return $false
}
function Health { try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7700/api/v1/health -TimeoutSec 3).StatusCode -eq 200 } catch { $false } }
function Engine-Log { Get-ChildItem "$data\logs\ductus*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime | ForEach-Object { Get-Content $_ } }
# Our events since the demo began, newest first, as Event Viewer shows them ("[1000] Ductus ...").
# Read in a child process: formatting them loads ductus.exe as the message file, and a process
# holding it makes every later uninstall and upgrade wait in InstallValidate (152 s against 1 s,
# measured with Restart Manager off by policy).
function Event-Messages([int] $id = 0) {
    $filter = "@{ LogName = 'Application'; ProviderName = 'Ductus'; StartTime = [datetime]::Parse('$($began.ToString("o"))')$(if ($id) { "; Id = $id" }) }"
    $json = powershell -NoProfile -Command "@(Get-WinEvent -FilterHashtable $filter -ErrorAction SilentlyContinue | ForEach-Object { '[' + `$_.Id + '] ' + `$_.Message }) | ConvertTo-Json"
    if (-not $json) { return @() }
    @(($json | Out-String) | ConvertFrom-Json)
}
function Has-Event([int] $id, [string] $text) { @(Event-Messages $id | Where-Object { $_ -match $text }).Count -gt 0 }
function Runs-As { ((sc.exe qc Ductus) -join "`n") -match "SERVICE_START_NAME\s*:\s*(.+)" | Out-Null; $Matches[1].Trim() }
function Process-Owner {
    $id = (Get-CimInstance Win32_Service -Filter "Name='Ductus'").ProcessId
    if (-not $id) { return "" }
    $o = Get-CimInstance Win32_Process -Filter "ProcessId=$id" | Invoke-CimMethod -MethodName GetOwner
    "$($o.Domain)\$($o.User)"
}
# Whether the local policy gives an account "Log on as a service" itself.
function Has-Logon-Right([string] $name) {
    # `.\name` is the service manager's way of writing this computer's accounts; a lookup needs its name.
    $lookup = $name -replace '^\.\\', "$env:COMPUTERNAME\"
    $sid = (New-Object System.Security.Principal.NTAccount($lookup)).Translate([System.Security.Principal.SecurityIdentifier]).Value
    $cfg = "$work\rights.inf"
    secedit /export /areas USER_RIGHTS /cfg $cfg | Out-Null
    $line = Get-Content $cfg | Where-Object { $_ -match "^SeServiceLogonRight" }
    # Listed by SID (`*S-1-5-...`), or by name for some accounts.
    return [bool](($line -match [regex]::Escape("*$sid")) -or ($line -match [regex]::Escape($name.TrimStart(".", "\"))))
}
# A native program's stderr is an error record in PowerShell 5.1: under "Stop" it would throw.
function Native([scriptblock] $run) {
    $ErrorActionPreference = "Continue"
    $out = & $run 2>&1 | ForEach-Object { "$_" } | Out-String
    return @{ Ok = ($LASTEXITCODE -eq 0); Out = $out }
}
function Set-Account([string[]] $more = @()) {
    $r = Native { $password | & $exe service account ".\$account" --password-stdin @more }
    Write-Host $r.Out
    return $r.Ok
}

try {
    Write-Host "== 1. silent install"
    Check "msiexec /i /qn succeeds" ((Msi @("/i", "`"$Msi`"") "install") -in 0, 3010)
    $service = Get-Service Ductus -ErrorAction SilentlyContinue
    Check "service registered" ($null -ne $service)
    Check "not started (nothing is configured yet)" ($service.Status -eq "Stopped")
    $config = (sc.exe qc Ductus) -join "`n"
    Check "runs as NT SERVICE\Ductus" ((Runs-As) -eq "NT SERVICE\Ductus")
    Check "starts automatically, with --service and the env file" (($config -match "AUTO_START") -and ($config -match "run --service --env-file"))
    Check "its own SID is unrestricted (the data folder's permissions hold for any account)" (((sc.exe qsidtype Ductus) -join "`n") -match "UNRESTRICTED")
    Check "Event Log source registered with our message file" ((Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Services\EventLog\Application\Ductus").EventMessageFile -match "ductus.exe")
    Check "installed files" ((Test-Path $exe) -and (Test-Path "$data\ductus.env") -and (Test-Path "$data\project") -and (Test-Path "$data\logs"))
    $acl = (icacls "$data\ductus.env") -join "`n"
    Check "settings readable by the service, not by users" (($acl -match "NT SERVICE\\Ductus") -and ($acl -notmatch "BUILTIN\\Users"))

    Write-Host "== 2. configure and start"
    & $exe init "$data\project" | Out-Null
    # The sample interface writes files; a service can't write in the project, so send to a receiver.
    $yaml = "$data\project\interfaces\adt-in\interface.yaml"
    (Get-Content $yaml -Raw) -replace 'destination: \{ file: \{ dir: out/adt-in \} \}', 'destination: { mllp: { host: 127.0.0.1, port: 6662 } }' | Set-Content $yaml -Encoding utf8
    Add-Content "$data\ductus.env" "`nDUCTUS_DATABASE_URL=$DatabaseUrl`n# marker kept across the upgrade"
    $sink = Start-Process $exe -ArgumentList "sink --port 6662" -PassThru -NoNewWindow -RedirectStandardOutput "$work\sink.log" -RedirectStandardError "$work\sink.err"
    Start-Service Ductus
    Check "service reaches Running" (Wait-Until { (Get-Service Ductus).Status -eq "Running" })
    Check "health answers" (Wait-Until { Health })
    # A project of its own with a SQL Server reached as the service's account (validate reads
    # resources.yaml only; nothing connects).
    & $exe init "$work\validate" | Out-Null
    Set-Content "$work\validate\resources.yaml" "databases:`n  billing: { sqlserver: { host: sql1.example.org, database: Billing, auth: windows } }`n" -Encoding utf8
    $validate = (Native { & $exe validate --root "$work\validate" }).Out
    Check "validate says who the service runs as" ($validate -match "service: runs as NT SERVICE\\Ductus \(on the network: ") $validate
    Check "and warns that its integrated login is the computer account's" ($validate -match "Windows login to SQL Server \(``billing``\) is the computer account") $validate

    Write-Host "== 3. a message round trip over MLLP"
    & $exe gen --to 127.0.0.1:6661 --count 100 --patients 10 | Out-Null
    Check "the receiver got all 100" (Wait-Until { (Get-Content "$work\sink.log" -ErrorAction SilentlyContinue | Out-String) -match "unique 100 " } 30)
    Check "the engine wrote its log" ((Engine-Log | Out-String) -match "started interfaces=")
    Check "Event Log: started, in our words (version, project, no password)" (Has-Event 1000 "Ductus .* started\.[\s\S]*project: ")
    Check "Event Log: the start event doesn't carry the database login" (-not (@(Event-Messages 1000) | Where-Object { $_ -match "@" }))

    Write-Host "== 4. clean stop"
    $at = Get-Date
    Stop-Service Ductus
    Check "service stopped" ((Get-Service Ductus).Status -eq "Stopped")
    Check "stop took the clean path" ((Engine-Log | Out-String) -match "stopped took=") ("(" + ((Get-Date) - $at).TotalSeconds + " s)")
    $exit = (sc.exe query Ductus) -join "`n"
    Check "exit code 0" ($exit -match "WIN32_EXIT_CODE\s*:\s*0")
    Check "Event Log: stopped cleanly" (Has-Event 1001 "Ductus stopped cleanly")
    Check "a stop on purpose leaves the marker" (Test-Path "$data\logs\stopped")

    Write-Host "== 5. upgrade over a configured install that was running"
    Start-Service Ductus
    Check "running again" (Wait-Until { Health })
    Check "the start removed the marker" (-not (Test-Path "$data\logs\stopped"))
    Check "msiexec /i of the other build: a real upgrade" (Upgrade "upgrade")
    Check "service running after the upgrade" (Wait-Until { (Get-Service Ductus).Status -eq "Running" -and (Health) })
    Check "settings kept" ((Get-Content "$data\ductus.env" -Raw) -match "marker kept across the upgrade")
    Check "project kept" (Test-Path "$data\project\interfaces\adt-in\interface.yaml")
    & $exe gen --to 127.0.0.1:6661 --count 50 --offset 1000 | Out-Null
    Check "still takes messages" (Wait-Until { (Get-Content "$work\sink.log" -ErrorAction SilentlyContinue | Out-String) -match "unique 150 " } 30)

    Write-Host "== 6. an upgrade leaves a service stopped on purpose stopped"
    Stop-Service Ductus
    Check "msiexec /i of the other build: a real upgrade" (Upgrade "upgrade-stopped")
    Start-Sleep 5
    Check "still stopped after the upgrade" ((Get-Service Ductus).Status -eq "Stopped")
    Start-Service Ductus
    Check "starts when asked" (Wait-Until { Health })
    Stop-Service Ductus

    Write-Host "== 6b. a start that fails says why in the Event Log"
    # The file's first DUCTUS_DATABASE_URL wins: put a broken one first, for one start.
    $settings = Get-Content "$data\ductus.env" -Raw
    Set-Content "$data\ductus.env" ("DUCTUS_DATABASE_URL=not-a-database-url`n" + $settings) -Encoding utf8 -NoNewline
    try { Start-Service Ductus -ErrorAction Stop } catch { }
    Check "the service stops" (Wait-Until { (Get-Service Ductus).Status -eq "Stopped" } 30)
    Check "Event Log: could not start, with the reason" (Wait-Until { Has-Event 1003 "Ductus could not start\.[\s\S]*error: " } 15)
    Set-Content "$data\ductus.env" $settings -Encoding utf8 -NoNewline
    Start-Service Ductus
    Check "starts again once fixed" (Wait-Until { Health })

    Write-Host "== 7. run as a named local account (ductus service account)"
    # net.exe, not New-LocalUser: PowerShell 7 (CI) reaches that cmdlet only through Windows PowerShell.
    net user $account $password /add /expires:never /comment:"Ductus demo-windows.ps1" | Out-Null
    Check "a local account for it" ($LASTEXITCODE -eq 0)
    $wrong = Native { "not-the-password" | & $exe service account ".\$account" --password-stdin }
    Check "a wrong password is refused before the service changes" ((-not $wrong.Ok) -and ($wrong.Out -match "refused") -and ((Runs-As) -eq "NT SERVICE\Ductus")) $wrong.Out
    Check "the account is set, the running service restarted" (Set-Account)
    Check "the service runs as it" ((Runs-As) -eq ".\$account")
    Check "its process is the account's" ((Process-Owner) -match "\\$account$") (Process-Owner)
    Check "the account may log on as a service" (Has-Logon-Right ".\$account")
    Check "it still reads its settings and project (health)" (Wait-Until { Health })
    & $exe gen --to 127.0.0.1:6661 --count 50 --offset 2000 | Out-Null
    Check "and takes messages" (Wait-Until { (Get-Content "$work\sink.log" -ErrorAction SilentlyContinue | Out-String) -match "unique 200 " } 30)
    Check "Event Log: the change, by whom" (Has-Event 1007 "to: \.\\$account")
    $shown = (Native { & $exe service account }).Out
    Check "``ductus service account`` says who and that it has the right" (($shown -match [regex]::Escape(".\$account")) -and ($shown -match "may log on as a service")) $shown

    Write-Host "== 8. an upgrade keeps the account, and waits for its password"
    Check "msiexec /i of the other build: a real upgrade" (Upgrade "upgrade-account")
    Start-Sleep 5
    Check "the account is kept" ((Runs-As) -eq ".\$account")
    Check "left stopped (its password can't be kept)" ((Get-Service Ductus).Status -eq "Stopped")
    Check "Event Log: what to run, in our words" (Has-Event 1005 "ductus service account \.\\$account --start")
    Check "its password given again, started" (Set-Account @("--start"))
    Check "running again as it" ((Wait-Until { Health }) -and ((Process-Owner) -match "\\$account$"))
    Stop-Service Ductus

    Write-Host "== 9. uninstall"
    # The receiver runs from the installed program: let go of it first, like an administrator would.
    if ($sink -and -not $sink.HasExited) { Stop-Process -Id $sink.Id -Force; $sink.WaitForExit(5000) | Out-Null }
    Check "msiexec /x succeeds" ((Msi @("/x", "`"$installed`"") "uninstall") -in 0, 3010)
    Check "service removed" ($null -eq (Get-Service Ductus -ErrorAction SilentlyContinue))
    Check "program removed" (-not (Test-Path $exe))
    Check "settings and project kept" ((Test-Path "$data\ductus.env") -and (Test-Path "$data\project"))

    Write-Host "== 10. install with SERVICEACCOUNT"
    Check "msiexec /i SERVICEACCOUNT=.\$account succeeds" ((Msi @("/i", "`"$Msi`"", "SERVICEACCOUNT=.\$account") "install-account") -in 0, 3010)
    Check "the service is installed as it" ((Runs-As) -eq ".\$account")
    Check "the installer granted it the logon right" (Has-Logon-Right ".\$account")
    Check "stopped" ((Get-Service Ductus).Status -eq "Stopped")
    Check "its password given, started" (Set-Account @("--start"))
    Check "runs as it" ((Wait-Until { Health }) -and ((Process-Owner) -match "\\$account$"))
    Stop-Service Ductus
    Check "msiexec /x succeeds" ((Msi @("/x", "`"$Msi`"") "uninstall-account") -in 0, 3010)
}
catch {
    Write-Host "EXCEPTION: $($_ | Out-String)"
    $failures += "exception"
}
finally {
    if ($sink -and -not $sink.HasExited) { Stop-Process -Id $sink.Id -Force }
    $ErrorActionPreference = "Continue" # net.exe's stderr (no such user) mustn't throw here
    net user $account /delete 2>&1 | Out-Null
    if ($failures.Count) {
        Write-Host "--- engine log"; Engine-Log | Select-Object -Last 40
        Write-Host "--- Event Log (source Ductus)"
        Event-Messages | Select-Object -First 15
        Write-Host "logs: $work"
    }
}
if ($failures.Count) { Write-Host "FAIL: $($failures -join ', ')"; exit 1 }
Write-Host "logs: $work"
Write-Host "PASS"
