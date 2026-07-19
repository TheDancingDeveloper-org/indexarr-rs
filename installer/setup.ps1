#Requires -Version 5.1
# Indexarr PostgreSQL setup script — bundled inside the NSIS installer
# Called by the installer with the paths it chose; never run directly by users.
param(
    [string]$InstallDir,
    [string]$DataDir,
    [string]$PgZip,
    [string]$PgVersion = "17.4",
    [string]$PgPort    = "5432",
    [string]$PgUser    = "indexarr",
    [string]$PgDb      = "indexarr"
)

$ErrorActionPreference = "Stop"

# Write a persistent install log — share this file if something goes wrong
$LogFile = "$InstallDir\install.log"
Start-Transcript -Path $LogFile -Force
Write-Host "Indexarr install log: $LogFile"

$pgBin  = "$InstallDir\pgsql\bin"
$pgData = "$DataDir\pgdata"

function Log($msg) { Write-Host "[indexarr-setup] $msg" }

# ── 1. Extract bundled PostgreSQL binaries ────────────────────────────────────
Log "Extracting PostgreSQL $PgVersion..."
Expand-Archive -Path $PgZip -DestinationPath $InstallDir -Force
Remove-Item $PgZip -ErrorAction SilentlyContinue

# ── 2. Initialise cluster ─────────────────────────────────────────────────────
$isNewCluster = -not (Test-Path "$pgData\PG_VERSION")
$pgPass = $null
$passTmp = "$env:TEMP\indexarr-pgpass.txt"
if ($isNewCluster) {
    Log "Initialising database cluster at $pgData..."
    New-Item -ItemType Directory -Force -Path $pgData | Out-Null
    if ((Get-ChildItem $pgData -Force | Select-Object -First 1)) {
        throw "Database directory is not empty but contains no PostgreSQL cluster: $pgData"
    }
    $rng    = New-Object System.Security.Cryptography.RNGCryptoServiceProvider
    $bytes  = New-Object byte[] 24
    $rng.GetBytes($bytes)
    $pgPass = [System.Convert]::ToBase64String($bytes)
    $pgPass | Set-Content -NoNewline $passTmp
    & "$pgBin\initdb.exe" -D $pgData -U postgres --pwfile $passTmp --encoding=UTF8 --locale=C
    if ($LASTEXITCODE -ne 0) { throw "initdb failed (exit $LASTEXITCODE)" }
} else {
    Log "Reusing existing database cluster at $pgData..."
}

# ── 3. Configure port ─────────────────────────────────────────────────────────
$pgConfig = "$pgData\postgresql.conf"
$config = Get-Content $pgConfig
$config = $config -replace '^\s*#?\s*port\s*=.*$', "port = $PgPort"
$config = $config -replace '^\s*#?\s*timezone\s*=.*$', "timezone = 'UTC'"
$config = $config -replace '^\s*#?\s*log_timezone\s*=.*$', "log_timezone = 'UTC'"
$config | Set-Content $pgConfig

# ── 4. Register + start PostgreSQL service ────────────────────────────────────
Log "Registering PostgreSQL service..."
$pgService = Get-Service IndexarrPostgres -ErrorAction SilentlyContinue
if (-not $pgService) {
    & "$pgBin\pg_ctl.exe" register -N IndexarrPostgres -D $pgData -S auto
    if ($LASTEXITCODE -ne 0) { throw "pg_ctl register failed (exit $LASTEXITCODE)" }
}
if ((Get-Service IndexarrPostgres).Status -ne 'Running') {
    Start-Service IndexarrPostgres
}

Log "Waiting for PostgreSQL to be ready..."
$ready = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    & "$pgBin\pg_isready.exe" -p $PgPort -U postgres 2>$null
    if ($LASTEXITCODE -eq 0) { $ready = $true; break }
}
if (-not $ready) { throw "PostgreSQL did not become ready within 30 seconds" }

# ── 5. Create role + database ─────────────────────────────────────────────────
if ($isNewCluster) {
    Log "Creating database..."
    $env:PGPASSWORD = $pgPass
    & "$pgBin\psql.exe" -p $PgPort -U postgres postgres -c "CREATE USER $PgUser WITH PASSWORD '$PgUser';"
    if ($LASTEXITCODE -ne 0) { throw "Failed to create role $PgUser" }
    & "$pgBin\psql.exe" -p $PgPort -U postgres postgres -c "CREATE DATABASE $PgDb OWNER $PgUser;"
    if ($LASTEXITCODE -ne 0) { throw "Failed to create database $PgDb" }
    $env:PGPASSWORD = ""
} else {
    Log "Existing database cluster retained."
}
Remove-Item $passTmp -ErrorAction SilentlyContinue

# ── 6. Write .env ─────────────────────────────────────────────────────────────
Log "Writing configuration..."
@"
INDEXARR_DB_URL=postgres://${PgUser}:${PgUser}@127.0.0.1:${PgPort}/${PgDb}
INDEXARR_DATA_DIR=${DataDir}
"@ | Set-Content "$InstallDir\.env" -Encoding UTF8

Log "Setup complete."
Stop-Transcript
