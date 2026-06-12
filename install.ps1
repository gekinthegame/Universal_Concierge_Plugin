# Universal Concierge Plugin — one-line installer (Windows, PowerShell).
#
#   irm https://github.com/gekinthegame/Universal_Concierge_Plugin/releases/latest/download/install.ps1 | iex
#
# Downloads the prebuilt concierge-plugin.exe from the latest GitHub Release,
# verifies its checksum, and installs it to %LOCALAPPDATA%\Programs\concierge-plugin
# (added to your user PATH). No separate mem, database, or cloud; Kubo/IPFS is
# optional (only for publishing / the Sidekick).

$ErrorActionPreference = "Stop"

$Repo = if ($env:CONCIERGE_REPO) { $env:CONCIERGE_REPO } else { "gekinthegame/Universal_Concierge_Plugin" }

$asset = "concierge-plugin-windows-x64"
$base  = "https://github.com/$Repo/releases/latest/download"
$dest  = Join-Path $env:LOCALAPPDATA "Programs\concierge-plugin"
$tmp   = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Force -Path $tmp  | Out-Null
New-Item -ItemType Directory -Force -Path $dest | Out-Null

try {
  Write-Host "Downloading $asset from $Repo ..."
  $zip = Join-Path $tmp "$asset.zip"
  Invoke-WebRequest -Uri "$base/$asset.zip" -OutFile $zip

  # Verify checksum (best-effort; skips if SHASUMS256.txt is absent).
  try {
    $sums = Invoke-WebRequest -Uri "$base/SHASUMS256.txt" -UseBasicParsing
    $line = ($sums.Content -split "`n" | Where-Object { $_ -match "$asset\.zip" } | Select-Object -First 1)
    if ($line) {
      $expected = ($line -split '\s+')[0].ToLower()
      $actual   = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
      if ($expected -ne $actual) { throw "Checksum mismatch for $asset.zip." }
      Write-Host "Checksum OK."
    }
  } catch { if ($_.Exception.Message -like "*mismatch*") { throw } }

  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  $exe = Join-Path $tmp "$asset\concierge-plugin.exe"
  if (-not (Test-Path $exe)) { throw "concierge-plugin.exe not found in the archive." }
  Copy-Item $exe (Join-Path $dest "concierge-plugin.exe") -Force
  Write-Host "Installed -> $dest\concierge-plugin.exe"

  # Add to the user PATH if it isn't already there.
  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($userPath -notlike "*$dest*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dest", "User")
    Write-Host "Added $dest to your user PATH (restart your terminal)."
  }
  Write-Host ""
  Write-Host "Done. Start the explorer with:  concierge-plugin gui"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
