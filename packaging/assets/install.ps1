[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA "Programs\DeviceRail")
)

$ErrorActionPreference = "Stop"
if (-not [System.IO.Path]::IsPathFullyQualified($Prefix)) {
    throw "install prefix must be an absolute path"
}

$sourceDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$destination = Join-Path $Prefix "bin"
New-Item -ItemType Directory -Force -Path $destination | Out-Null
Copy-Item -LiteralPath (Join-Path $sourceDirectory "bin\devicerail-daemon.exe") -Destination $destination
Copy-Item -LiteralPath (Join-Path $sourceDirectory "bin\devicerail-bundle.exe") -Destination $destination

Write-Output "Installed DeviceRail binaries in $destination"
