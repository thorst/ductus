# SQL Server Express with Windows logins, for the Windows-login tests (mssql.rs, doctor.rs):
# CI's `windows-tests` job, or a developer's machine (as administrator).
#
#   scripts/dev/sqlexpress.ps1 -Password <pw> [-Install] [-Port 1433] [-AllowWsl] [-Done <file>]
#
# -Install    installs SQL Server 2022 Express (choco, ~3 min) if no SQLEXPRESS instance exists.
# Then, every run: TCP on -Port, SQL logins allowed (mixed mode), and
#   - a login for whoever runs this (the tests' integrated login),
#   - local account `ductus-sql-named` with a login (a named Windows account),
#   - local account `ductus-sql-nologin` with none (the refusal's test),
#   - SQL login `ductus_sql` (to time a SQL login against a Windows one),
# all with -Password. -AllowWsl opens the port to WSL's network (the NTLM check from Linux).
# -Done: a file written at the end with `ready` or `failed: <why>` (CI runs this in the background).
# Where WSL runs the Linux test SQL Server (sqlserver.sh), Windows's 1433 is WSL's forwarded port:
# give another (-Port 1435), or SQL Server Express won't start ("Only one usage of each socket address").
param(
    [Parameter(Mandatory)] [string] $Password,
    [switch] $Install,
    [int] $Port = 1433,
    [switch] $AllowWsl,
    [string] $Done
)
$ErrorActionPreference = "Stop"
# Started from pwsh 7 (CI's steps), Windows PowerShell inherits pwsh's module path and can't load
# its own modules (ConvertTo-SecureString, LocalAccounts): use the machine's.
if ($PSVersionTable.PSVersion.Major -le 5) {
    $env:PSModulePath = [Environment]::GetEnvironmentVariable("PSModulePath", "Machine")
}

function Instance-Key {
    $names = Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Microsoft SQL Server\Instance Names\SQL" -ErrorAction SilentlyContinue
    if ($names -and $names.SQLEXPRESS) { "HKLM:\SOFTWARE\Microsoft\Microsoft SQL Server\$($names.SQLEXPRESS)" }
}

function Sql([string] $text) {
    # Integrated, as the administrator running this (on this machine: shared memory, not TCP).
    $connection = New-Object System.Data.SqlClient.SqlConnection "Server=.\SQLEXPRESS;Integrated Security=true;Database=master"
    $connection.Open()
    try {
        $command = $connection.CreateCommand()
        $command.CommandText = $text
        [void] $command.ExecuteNonQuery()
    } finally { $connection.Close() }
}

try {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    if (-not (Instance-Key)) {
        if (-not $Install) { throw "no SQLEXPRESS instance: run with -Install" }
        choco install sql-server-express -y --no-progress
        if ($LASTEXITCODE -ne 0) { throw "choco install sql-server-express failed ($LASTEXITCODE)" }
        "installed in $($sw.Elapsed)"
    }
    $key = Instance-Key
    if (-not $key) { throw "SQL Server Express installed, but no SQLEXPRESS instance in the registry" }

    # A fixed TCP port (not a dynamic one) and mixed mode; both take a restart.
    $tcp = "$key\MSSQLServer\SuperSocketNetLib\Tcp"
    Set-ItemProperty $tcp -Name Enabled -Value 1
    Set-ItemProperty "$tcp\IPAll" -Name TcpDynamicPorts -Value ""
    Set-ItemProperty "$tcp\IPAll" -Name TcpPort -Value "$Port"
    Set-ItemProperty "$key\MSSQLServer" -Name LoginMode -Value 2
    Restart-Service 'MSSQL$SQLEXPRESS' -Force

    $secure = ConvertTo-SecureString $Password -AsPlainText -Force
    foreach ($account in "ductus-sql-named", "ductus-sql-nologin") {
        if (Get-LocalUser $account -ErrorAction SilentlyContinue) {
            Set-LocalUser $account -Password $secure -PasswordNeverExpires $true
        } else {
            New-LocalUser $account -Password $secure -PasswordNeverExpires -Description "Ductus tests: SQL Server Windows logins" | Out-Null
        }
    }
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $named = "$env:COMPUTERNAME\ductus-sql-named"
    $quoted = $Password.Replace("'", "''")
    foreach ($login in $me, $named) {
        Sql "IF SUSER_ID(N'$login') IS NULL CREATE LOGIN [$login] FROM WINDOWS"
    }
    Sql "IF SUSER_ID(N'ductus_sql') IS NULL CREATE LOGIN ductus_sql WITH PASSWORD = N'$quoted', CHECK_POLICY = OFF ELSE ALTER LOGIN ductus_sql WITH PASSWORD = N'$quoted'"
    # The no-login account must not have one (an earlier run's would make the test pass for the wrong reason).
    Sql "IF SUSER_ID(N'$env:COMPUTERNAME\ductus-sql-nologin') IS NOT NULL DROP LOGIN [$env:COMPUTERNAME\ductus-sql-nologin]"
    # Nor one through a group: Express gives BUILTIN\Users a login, and every authenticated account
    # is in BUILTIN\Users (through Authenticated Users). This test server's logins are all named.
    Sql "IF SUSER_ID(N'BUILTIN\Users') IS NOT NULL DROP LOGIN [BUILTIN\Users]"

    if ($AllowWsl -and -not (Get-NetFirewallRule -DisplayName "Ductus tests: SQL Server from WSL" -ErrorAction SilentlyContinue)) {
        $wsl = Get-NetIPAddress -AddressFamily IPv4 | Where-Object InterfaceAlias -like "vEthernet (WSL*"
        if (-not $wsl) { throw "no WSL network (vEthernet (WSL...)) found: is WSL running in NAT mode?" }
        $subnet = "$($wsl.IPAddress)/$($wsl.PrefixLength)"
        New-NetFirewallRule -DisplayName "Ductus tests: SQL Server from WSL" -Direction Inbound -Protocol TCP -LocalPort $Port -RemoteAddress $subnet -Action Allow | Out-Null
        "firewall: $Port open to $subnet"
    }
    "ready in $($sw.Elapsed) on port ${Port}: logins for $me, $named; none for $env:COMPUTERNAME\ductus-sql-nologin"
    if ($Done) { "ready" | Set-Content $Done }
} catch {
    "failed: $_"
    if ($Done) { "failed: $_" | Set-Content $Done }
    exit 1
}
