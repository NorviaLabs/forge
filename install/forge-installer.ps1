# Forge is not supported natively on Windows.
#
# The sandbox that confines agent-spawned commands is built on Seatbelt
# (macOS) and bubblewrap (Linux). Native Windows would need a third backend —
# restricted tokens, filesystem ACLs and synthetic SIDs, the approach Codex
# uses — and forge does not have one. Rather than ship a binary whose
# permission model silently has no enforcement floor underneath it, forge runs
# under WSL2 and uses the Linux sandbox. Claude Code takes the same position.
#
# This script used to install a native x86_64-pc-windows-msvc build. That
# target is no longer produced, so it now points you at the supported path
# instead of installing something unsupported.

$ErrorActionPreference = "Stop"

function Test-Wsl2 {
    try {
        $null = & wsl.exe --status 2>$null
        return $LASTEXITCODE -eq 0
    } catch {
        return $false
    }
}

Write-Host ""
Write-Host "forge does not ship a native Windows build." -ForegroundColor Yellow
Write-Host "It runs under WSL2, where it uses the same sandbox as Linux."
Write-Host ""

if (-not (Test-Wsl2)) {
    Write-Host "WSL2 was not detected. Install it first:" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "    wsl --install"
    Write-Host ""
    Write-Host "Then re-run this script, or install forge from inside your WSL2 shell."
    exit 1
}

Write-Host "WSL2 detected. Install forge from inside your WSL2 distribution:" -ForegroundColor Cyan
Write-Host ""
Write-Host "    wsl -- bash -c 'curl -fsSL https://raw.githubusercontent.com/NorviaLabs/forge/main/install/forge-installer.sh | bash'"
Write-Host ""
Write-Host "The sandbox additionally needs bubblewrap inside that distribution:"
Write-Host ""
Write-Host "    wsl -- sudo apt-get install -y bubblewrap"
Write-Host ""
Write-Host "Without it forge still runs, but every shell command asks for approval,"
Write-Host "because there is no enforcement floor for Auto mode to sit on."
Write-Host ""
exit 1
