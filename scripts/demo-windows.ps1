# The Windows installer, end to end (needs an elevated PowerShell; CI runs it on windows-latest):
#   install silently -> service registered right -> configure -> start -> health -> MLLP round trip
#   -> MLLP over TLS, senders with a client certificate -> log and Event Log written -> clean stop
#   -> upgrade over a running, configured install, a sender sending through it (what it saw is
#   printed: refused, reset, longest wait; nothing lost) -> an upgrade leaves a service stopped on
#   purpose stopped -> going back: the installer refuses an older version, uninstall then install
#   it -> run as a named local account (`ductus service account`) -> an upgrade keeps the account
#   and waits for its password -> uninstall -> install with SERVICEACCOUNT -> uninstall.
#
# Upgrades need a second build of the installer (-UpgradeMsi): each build has its own ProductCode,
# so one over the other is a real (major) upgrade. The same file again is only a maintenance run of
# the installed product: nothing is removed or reinstalled, and it proves nothing about upgrades.
# Going back needs a build with a higher version (-NewerMsi, the next minor), standing in for the
# version a site upgraded to: the demo upgrades to it, then goes back to -Msi.
#
#   .\scripts\demo-windows.ps1 -Msi dist\ductus-0.6.0-x64.msi -UpgradeMsi dist2\ductus-0.6.0-x64.msi -NewerMsi dist3\ductus-0.7.0-x64.msi -DatabaseUrl postgres://u:p@localhost/db
param(
    [Parameter(Mandatory)] [string] $Msi,
    [Parameter(Mandatory)] [string] $UpgradeMsi,
    [Parameter(Mandatory)] [string] $NewerMsi,
    [Parameter(Mandatory)] [string] $DatabaseUrl,
    # Where the installer logs and the receiver's output go (CI uploads it).
    [string] $LogDir = ""
)
$ErrorActionPreference = "Stop"
$Msi = (Resolve-Path $Msi).Path
$UpgradeMsi = (Resolve-Path $UpgradeMsi).Path
$NewerMsi = (Resolve-Path $NewerMsi).Path
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
# Senders over TLS from 3b on: the lab's listener certificate trusted, the sender's presented.
$tls = @()
# How many distinct messages the receiver should have by now.
$expected = 0
# The receiver's latest count line ("... | unique N | duplicates N | out of order N | ...").
function Received { Get-Content "$work\sink.log" -ErrorAction SilentlyContinue | Where-Object { $_ -match "unique \d+ " } | Select-Object -Last 1 }
function Send([int] $count, [int] $offset, [string] $name) {
    & $tools gen @tls --count $count --offset $offset | Out-Null
    $script:expected += $count
    Check $name (Wait-Until { (Received) -match "unique $script:expected " } 30) (Received)
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
    # The receiver and sender run from a copy: an upgrade replaces the installed ductus.exe, and
    # Restart Manager closes whatever runs it, here with the console it shares with this script.
    $tools = Join-Path $work "ductus-tools.exe"
    Copy-Item $exe $tools -Force
    $sink = Start-Process $tools -ArgumentList "sink --port 6662" -PassThru -NoNewWindow -RedirectStandardOutput "$work\sink.log" -RedirectStandardError "$work\sink.err"
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
    Send 100 0 "the receiver got all 100"
    Check "the engine wrote its log" ((Engine-Log | Out-String) -match "started interfaces=")
    Check "Event Log: started, in our words (version, project, no password)" (Has-Event 1000 "Ductus .* started\.[\s\S]*project: ")
    Check "Event Log: the start event doesn't carry the database login" (-not (@(Event-Messages 1000) | Where-Object { $_ -match "@" }))

    Write-Host "== 3b. MLLP over TLS, senders with a client certificate"
    # Lab certificates: the listener's, and a sender's that the listener takes as its clients' CA
    # (a self-signed certificate is its own CA). On a server both come from the hospital's CA
    # (first-interface.md; demo-v06.sh does it that way on Linux).
    & $exe cert self-signed "$data\project\tls\mllp" --name 127.0.0.1 --name localhost | Out-Null
    & $exe cert self-signed "$work\sender" --name sender.test | Out-Null
    Copy-Item "$work\sender.pem" "$data\project\tls\sender-ca.pem"
    Set-Content "$data\project\resources.yaml" "certificates:`n  ductus-mllp: { cert_file: tls/mllp.pem, key_file: tls/mllp.key }`n  senders: { ca_file: tls/sender-ca.pem }`n" -Encoding utf8
    (Get-Content $yaml -Raw) -replace 'source: \{ mllp: \{ port: 6661 \} \}', 'source: { mllp: { port: 6661, tls: { certificate: ductus-mllp, clients: { ca: senders, subjects: ["CN=sender.test"] } } } }' | Set-Content $yaml -Encoding utf8
    $ports = (Native { & $exe ports --root "$data\project" }).Out
    Check "ductus ports: the listener is MLLP over TLS" ($ports -match "MLLP over TLS") $ports
    Restart-Service Ductus
    Check "running again, TLS on the listener" (Wait-Until { Health })
    $script:tls = @("--to", "127.0.0.1:6661", "--ca", "$data\project\tls\mllp.pem", "--cert", "$work\sender.pem", "--key", "$work\sender.key")
    $sent = (Native { & $tools gen @tls --count 100 --offset 500 }).Out
    Write-Host "   $($sent.Trim())"
    $script:expected += 100
    Check "a sender with its certificate: all 100 received" (Wait-Until { (Received) -match "unique $expected " } 30) (Received)
    # A sender without TLS gets no ACK (it would retry for ever: stopped after 5 s).
    $plain = Start-Process $tools -ArgumentList "gen --to 127.0.0.1:6661 --count 1 --offset 900" -PassThru -NoNewWindow -RedirectStandardOutput "$work\gen-plain.log" -RedirectStandardError "$work\gen-plain.err"
    Start-Sleep 5
    if (-not $plain.HasExited) { Stop-Process -Id $plain.Id -Force }
    $said = (Get-Content "$work\gen-plain.log", "$work\gen-plain.err" -ErrorAction SilentlyContinue | Out-String)
    Check "a sender without TLS is refused" (($said -match "connection closed before ACK") -and ((Received) -match "unique $expected ")) $said

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
    # A sender through the upgrade, as an EHR would keep sending: 4 kept connections, 200
    # messages/s for a minute, the installer started 3 s in. The engine is down while the installer
    # stops, replaces and starts it: what the sender saw is printed (the gap is the documented
    # behaviour); only "nothing lost" is judged.
    $began_sending = Get-Date
    $through = Start-Process $tools -ArgumentList (@("gen") + $tls + @("--count", "12000", "--patients", "100", "--connections", "4", "--rate", "200", "--offset", "10000")) `
        -PassThru -NoNewWindow -RedirectStandardOutput "$work\gen-upgrade.log" -RedirectStandardError "$work\gen-upgrade.err"
    Start-Sleep 3
    $at = Get-Date
    Check "msiexec /i of the other build: a real upgrade" (Upgrade "upgrade")
    $msi_took = ((Get-Date) - $at).TotalSeconds
    Check "service running after the upgrade" (Wait-Until { (Get-Service Ductus).Status -eq "Running" -and (Health) })
    Check "the sender finished" ($through.WaitForExit(180000))
    $sending_took = ((Get-Date) - $began_sending).TotalSeconds
    $script:expected += 12000
    Check "nothing lost through the upgrade" (Wait-Until { (Received) -match "unique $expected " } 60) (Received)
    $line = Get-Content "$work\gen-upgrade.log" -ErrorAction SilentlyContinue | Select-Object -Last 1
    Write-Host ("   the installer ran from 3.0 s to {0:N1} s; the sender sent from 0 to {1:N1} s" -f (3 + $msi_took), $sending_took)
    Write-Host "   the sender through the upgrade: $line"
    Write-Host "   the receiver: $(Received)"
    Check "the upgrade fell inside the sending" ((3 + $msi_took) -lt $sending_took)
    Check "settings kept" ((Get-Content "$data\ductus.env" -Raw) -match "marker kept across the upgrade")
    Check "project kept" (Test-Path "$data\project\interfaces\adt-in\interface.yaml")
    Send 50 1000 "still takes messages"

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

    Write-Host "== 6c. going back to an older version: uninstall, then install it"
    # Up to the next version first, as a site would have; the database stays as that version
    # left it, and the older one runs on it (migrations only add: upgrade.md, "Going back").
    $older = $script:installed
    Check "msiexec /i of the newer version: a real upgrade" (((Msi @("/i", "`"$NewerMsi`"") "upgrade-newer") -in 0, 3010) -and ((Get-Content "$work\upgrade-newer.log" -Raw) -match "Product: Ductus -- Installation completed successfully"))
    Check "running on the newer version" (Wait-Until { (Get-Service Ductus).Status -eq "Running" -and (Health) })
    $refused = Msi @("/i", "`"$older`"") "downgrade-refused"
    Check "the installer refuses the older version over it, and says how to go back" (($refused -notin 0, 3010) -and ((Get-Content "$work\downgrade-refused.log" -Raw) -match "A newer version of Ductus is already installed\. Uninstall it first to go back\.")) "exit $refused"
    Check "the newer version still runs" (Health)
    Check "msiexec /x of the newer version" ((Msi @("/x", "`"$NewerMsi`"") "downgrade-uninstall") -in 0, 3010)
    Check "settings and project kept" ((Test-Path "$data\ductus.env") -and (Test-Path "$data\project\resources.yaml"))
    Check "msiexec /i of the older version" ((Msi @("/i", "`"$older`"") "downgrade-install") -in 0, 3010)
    $script:installed = $older
    Check "installed, not started (as any install)" ((Get-Service Ductus).Status -eq "Stopped")
    Start-Service Ductus
    Check "the older version starts on the database the newer one used" (Wait-Until { Health })
    Send 50 3000 "and takes messages over TLS again"

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
    Send 50 2000 "and takes messages"
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
