# Universal Concierge Plugin — one-line installer (Windows, PowerShell).
#
#   irm https://github.com/gekinthegame/Universal_Concierge_Plugin/releases/latest/download/install.ps1 | iex
#
# Downloads the prebuilt Concierge binaries from the latest GitHub Release,
# verifies the checksum, and installs them to %LOCALAPPDATA%\Programs\concierge-plugin
# (added to your user PATH). No separate mem, database, or cloud; Kubo/IPFS is
# optional (only for publishing / the Sidekick).

$ErrorActionPreference = "Stop"

$Repo = if ($env:CONCIERGE_REPO) { $env:CONCIERGE_REPO } else { "gekinthegame/Universal_Concierge_Plugin" }

# Recommend a wallet browser first (Decision 0033): Brave (fuller) or Opera.
$braveExe = @(
  "$env:ProgramFiles\BraveSoftware\Brave-Browser\Application\brave.exe",
  "${env:ProgramFiles(x86)}\BraveSoftware\Brave-Browser\Application\brave.exe",
  "$env:LOCALAPPDATA\BraveSoftware\Brave-Browser\Application\brave.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
$operaExe = @(
  "$env:LOCALAPPDATA\Programs\Opera\opera.exe",
  "$env:ProgramFiles\Opera\opera.exe",
  "${env:ProgramFiles(x86)}\Opera\opera.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($braveExe) {
  Write-Host "Brave detected - full Concierge experience (wallet, native IPFS, bookmark memory)."
} elseif ($operaExe) {
  Write-Host "Opera detected - wallet + bookmark memory (IPFS via gateway; Brave adds native IPFS)."
} else {
  Write-Host ""
  Write-Host "The Concierge works best in a Chromium wallet browser (pick one):"
  Write-Host "  1) Brave  (recommended - wallet, native ipfs://, bookmark memory)  https://brave.com/download/"
  Write-Host "  2) Opera  (built-in wallet, bookmark memory; IPFS via gateway)     https://www.opera.com/download/"
  Write-Host "Strongly recommended (not required)."
  $reply = Read-Host "Open a download page now? [1=Brave / 2=Opera / N=skip]"
  if ($reply -eq '1') { Start-Process "https://brave.com/download/" }
  elseif ($reply -eq '2') { Start-Process "https://www.opera.com/download/" }
  Write-Host ""
}

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
  $kernelExe = Join-Path $tmp "$asset\concierge-kernel.exe"
  if (-not (Test-Path $exe)) { throw "concierge-plugin.exe not found in the archive." }
  if (-not (Test-Path $kernelExe)) { throw "concierge-kernel.exe not found in the archive." }
  Copy-Item $exe (Join-Path $dest "concierge-plugin.exe") -Force
  Copy-Item $kernelExe (Join-Path $dest "concierge-kernel.exe") -Force
  Write-Host "Installed -> $dest\concierge-plugin.exe"
  Write-Host "Installed -> $dest\concierge-kernel.exe"

  # Connect to Claude Code as an MCP server (best-effort).
  Write-Host ""
  try { & (Join-Path $dest "concierge-plugin.exe") setup } catch {}

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
