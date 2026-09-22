[CmdletBinding()]
param(
    [string]$Repo = (Resolve-Path "$PSScriptRoot\..\.."),
    [string]$OutputDirectory,
    [string]$ExpectedVersion,
    [switch]$SkipBuild
)

# KasaLite 의 Windows 설치본(MSI)을 만든다. 라이트는 자동 업데이트·웹뷰·학생
# 로스터가 없어서 본판 kasaterm 의 패키징에서 WinSparkle·arona-ui·collab-hooks
# 세 갈래가 통째로 빠진다 — 깔리는 것은 exe 둘과 한글 폰트뿐이다.
#
#   pwsh scripts\windows\package.ps1 [-ExpectedVersion v0.1.0] [-SkipBuild]
#
# 만든 MSI 는 다시 풀어(dark.exe) 들어가야 할 파일이 실제로 들어갔는지 본다.
# 매니페스트에만 있고 payload 가 없는 MSI 는 설치까지 가서야 티가 나서다.

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Invoke-External {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$ArgumentList
    )

    & $FilePath @ArgumentList
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath failed with exit code $LASTEXITCODE"
    }
}

function Reset-Directory {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$AllowedRoot
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $fullRoot = [IO.Path]::GetFullPath($AllowedRoot).TrimEnd('\') + '\'
    if (-not $fullPath.StartsWith($fullRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to reset a directory outside $fullRoot`: $fullPath"
    }
    if (Test-Path -LiteralPath $fullPath) {
        Remove-Item -LiteralPath $fullPath -Recurse -Force
    }
    New-Item -ItemType Directory -Path $fullPath -Force | Out-Null
    return $fullPath
}

function Get-VerifiedDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$Sha256
    )

    if (Test-Path -LiteralPath $Destination) {
        $actual = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash
        if ($actual -eq $Sha256) {
            return
        }
        Remove-Item -LiteralPath $Destination -Force
    }

    Write-Host "-- download: $Uri"
    Invoke-WebRequest -Uri $Uri -OutFile $Destination
    $actual = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash
    if ($actual -ne $Sha256) {
        Remove-Item -LiteralPath $Destination -Force
        throw "checksum mismatch for $Uri"
    }
}

function Assert-MsiManifest {
    param(
        [Parameter(Mandatory = $true)][string]$Manifest,
        [Parameter(Mandatory = $true)][string[]]$Names
    )

    [xml]$document = Get-Content -LiteralPath $Manifest -Encoding utf8
    $fileNodes = @($document.SelectNodes("//*[local-name()='File']"))
    foreach ($name in $Names) {
        $node = $fileNodes | Where-Object { $_.Name -eq $name } | Select-Object -First 1
        if (-not $node) {
            throw "MSI verification failed: $name is missing from the manifest"
        }
        if (-not (Test-Path -LiteralPath $node.Source)) {
            throw "MSI verification failed: payload for $name was not extracted"
        }
    }
}

$repoRoot = (Resolve-Path -LiteralPath $Repo).Path
$targetRoot = Join-Path $repoRoot "target"
$releaseRoot = Join-Path $targetRoot "release"
$workRoot = Join-Path $targetRoot "package-windows-x64"
$toolsRoot = Join-Path $targetRoot "package-tools"
$downloadsRoot = Join-Path $toolsRoot "downloads"
$distRoot = if ($OutputDirectory) {
    [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputDirectory))
} else {
    Join-Path $repoRoot "dist"
}

Set-Location $repoRoot
New-Item -ItemType Directory -Path $downloadsRoot -Force | Out-Null
New-Item -ItemType Directory -Path $distRoot -Force | Out-Null

$versionLine = Select-String -LiteralPath (Join-Path $repoRoot "Cargo.toml") -Pattern '^version = "([0-9]+\.[0-9]+\.[0-9]+)"' | Select-Object -First 1
if (-not $versionLine) {
    throw "workspace version not found in Cargo.toml"
}
$version = $versionLine.Matches[0].Groups[1].Value
if ($ExpectedVersion) {
    $normalizedExpected = $ExpectedVersion.TrimStart('v')
    if ($normalizedExpected -ne $version) {
        throw "Cargo.toml version ($version) does not match expected version ($ExpectedVersion)"
    }
}

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    throw "this package currently supports x64 Windows only; host architecture is $env:PROCESSOR_ARCHITECTURE"
}

if (-not $SkipBuild) {
    Write-Host "-- build release binaries"
    Invoke-External -FilePath "cargo.exe" -ArgumentList @("build", "--release", "-p", "kasaterm", "--bin", "kasaterm")
    Invoke-External -FilePath "cargo.exe" -ArgumentList @("build", "--release", "-p", "kasa-socket", "--bin", "kasaterm-cli")
}

$appExe = Join-Path $releaseRoot "kasaterm.exe"
$cliExe = Join-Path $releaseRoot "kasaterm-cli.exe"
foreach ($artifact in @($appExe, $cliExe)) {
    if (-not (Test-Path -LiteralPath $artifact)) {
        throw "build artifact is missing: $artifact"
    }
}

# 라이트 판정은 실행 파일 이름으로 하지 않지만(detect 가 늘 lite 다), 맥 번들이
# kasaterm-lite 라는 이름으로 깔리므로 Windows 도 같은 이름으로 맞춘다 — 한 기계에
# 본판과 함께 깔렸을 때 작업 관리자에서 어느 쪽인지 바로 갈린다.
$stageBin = Reset-Directory -Path (Join-Path $workRoot "bin") -AllowedRoot $targetRoot
Copy-Item -LiteralPath $appExe -Destination (Join-Path $stageBin "kasaterm-lite.exe") -Force
Copy-Item -LiteralPath $cliExe -Destination (Join-Path $stageBin "kasaterm-cli.exe") -Force

$wixVersion = "3.14.1"
$wixZip = Join-Path $downloadsRoot "wix314-binaries.zip"
Get-VerifiedDownload `
    -Uri "https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip" `
    -Destination $wixZip `
    -Sha256 "6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31"

$wixRoot = Join-Path $toolsRoot "wix-$wixVersion"
if (-not (Test-Path -LiteralPath (Join-Path $wixRoot "candle.exe"))) {
    $wixRoot = Reset-Directory -Path $wixRoot -AllowedRoot $targetRoot
    Expand-Archive -LiteralPath $wixZip -DestinationPath $wixRoot -Force
}
$candleExe = Join-Path $wixRoot "candle.exe"
$lightExe = Join-Path $wixRoot "light.exe"
$darkExe = Join-Path $wixRoot "dark.exe"
$wixUiExtension = Join-Path $wixRoot "WixUIExtension.dll"
foreach ($tool in @($candleExe, $lightExe, $darkExe, $wixUiExtension)) {
    if (-not (Test-Path -LiteralPath $tool)) {
        throw "WiX tool is missing: $tool"
    }
}

$wixBuildRoot = Reset-Directory -Path (Join-Path $workRoot "wix") -AllowedRoot $targetRoot
$mainWxs = Join-Path $repoRoot "app\kasaterm\wix\main.wxs"
$mainObj = Join-Path $wixBuildRoot "main.wixobj"
Invoke-External -FilePath $candleExe -ArgumentList @(
    "-nologo", "-arch", "x64", "-dVersion=$version", "-dStageBinDir=$stageBin",
    "-out", $mainObj, $mainWxs
)

$baseName = "kasalite-v$version-windows-x86_64"
$msiPath = Join-Path $distRoot "$baseName.msi"
$wixPdbPath = [IO.Path]::ChangeExtension($msiPath, ".wixpdb")
if (Test-Path -LiteralPath $wixPdbPath) {
    Remove-Item -LiteralPath $wixPdbPath -Force
}
Invoke-External -FilePath $lightExe -ArgumentList @(
    "-nologo", "-spdb", "-ext", $wixUiExtension, "-cultures:en-us", "-out", $msiPath, $mainObj
)

$verifyRoot = Reset-Directory -Path (Join-Path $workRoot "verify") -AllowedRoot $targetRoot
$verifyFiles = Join-Path $verifyRoot "files"
$verifyWxs = Join-Path $verifyRoot "package.wxs"
Invoke-External -FilePath $darkExe -ArgumentList @(
    "-nologo", "-x", $verifyFiles, "-o", $verifyWxs, $msiPath
)
Assert-MsiManifest -Manifest $verifyWxs -Names @(
    "kasaterm-lite.exe", "kasaterm-cli.exe", "NotoSansKR-Variable.ttf", "OFL-NotoSansKR.txt"
)

# 본판에서 걷어낸 것들이 매니페스트에 되살아나 있으면 세운다 — wxs 를 본판에서
# 다시 옮겨 올 때 조용히 딸려 들어오는 것이 정확히 이 셋이다.
[xml]$verifyDoc = Get-Content -LiteralPath $verifyWxs -Encoding utf8
$packagedNames = @($verifyDoc.SelectNodes("//*[local-name()='File']") | ForEach-Object { $_.Name })
foreach ($forbidden in @("WinSparkle.dll", "characters.json", "index.html")) {
    if ($packagedNames -contains $forbidden) {
        throw "lite MSI contains $forbidden"
    }
}

$hash = (Get-FileHash -LiteralPath $msiPath -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath "$msiPath.sha256" -Value "$hash  $([IO.Path]::GetFileName($msiPath))" -Encoding ascii

Write-Host ""
Write-Host "Windows package verified:"
Write-Host "  $msiPath"
