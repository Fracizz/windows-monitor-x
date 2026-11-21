param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\dist'),
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$buildArgs = @('build', '--release', '--locked', '--bin', 'windows-monitor', '--message-format=json-render-diagnostics')
if ($Offline) { $buildArgs += '--offline' }

function Get-RepositoryName {
    if (-not (Test-Path -LiteralPath '.git')) {
        return $null
    }
    if (Get-Command git -ErrorAction SilentlyContinue) {
        $remotes = & git remote
        if ($LASTEXITCODE -eq 0 -and $remotes -contains 'origin') {
            $origin = & git config --get remote.origin.url
            if ($LASTEXITCODE -eq 0 -and $origin) {
                $path = $origin.Trim().TrimEnd([char[]]'/\')
                $name = ($path -split '[/\\:]')[-1] -creplace '\.git$', ''
                if ($name) { return $name }
            }
        }

        $commonDirectory = & git rev-parse --path-format=absolute --git-common-dir
        if ($LASTEXITCODE -eq 0 -and $commonDirectory) {
            return Split-Path -Leaf (Split-Path -Parent $commonDirectory)
        }
    }
    return Split-Path -Leaf $project
}

$previousName = $env:WINDOWS_MONITOR_REPOSITORY_NAME
Push-Location -LiteralPath $project
try {
    $env:WINDOWS_MONITOR_REPOSITORY_NAME = Get-RepositoryName
    $messages = & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed (exit $LASTEXITCODE)." }
    $exe = $messages | ForEach-Object {
        if ($_.StartsWith('{')) {
            $artifact = $_ | ConvertFrom-Json
            if ($artifact.reason -eq 'compiler-artifact' -and $artifact.executable) {
                $artifact.executable
            }
        }
    } | Select-Object -Last 1
} finally {
    $env:WINDOWS_MONITOR_REPOSITORY_NAME = $previousName
    Pop-Location
}

if (-not $exe -or [IO.Path]::GetExtension($exe) -ine '.exe') {
    throw 'Cargo did not produce a Windows executable.'
}
$version = (Get-Item -LiteralPath $exe).VersionInfo
$name = $version.OriginalFilename
if (-not $version.ProductName -or $name -cne ($version.ProductName + '.exe') -or
    $name.IndexOfAny([IO.Path]::GetInvalidFileNameChars()) -ge 0 -or -not $name.EndsWith('-x.exe')) {
    throw 'The executable has invalid or inconsistent packaging metadata.'
}
[void][IO.Directory]::CreateDirectory($destination)
$package = Join-Path $destination $name
Copy-Item -LiteralPath $exe -Destination $package -Force
# Overwriting a file on Windows can leave the casing of its existing directory entry.
$entry = Get-ChildItem -LiteralPath $destination -Filter $name -File |
    Where-Object { $_.Name -ieq $name } |
    Sort-Object { $_.Name -cne $name } |
    Select-Object -First 1
if ($entry -and $entry.Name -cne $name) {
    Rename-Item -LiteralPath $entry.FullName -NewName $name
}
Get-Item -LiteralPath $package
