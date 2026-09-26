param(
    [string]$Output = (Join-Path $PSScriptRoot '..\..\dist\miac-control-skill-windows-x64.zip')
)

$ErrorActionPreference = 'Stop'
$files = [ordered]@{
    'miac-control/SKILL.md' = Join-Path $PSScriptRoot 'SKILL.md'
    'miac-control/references/setup.md' = Join-Path $PSScriptRoot 'references\setup.md'
    'miac-control/scripts/miac-assistant.exe' = Join-Path $PSScriptRoot 'scripts\miac-assistant.exe'
    'miac-control/scripts/miac-app.exe' = Join-Path $PSScriptRoot 'scripts\miac-app.exe'
}

foreach ($source in $files.Values) {
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "缺少打包文件：$source"
    }
}

$outputDir = Split-Path -Parent $Output
New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
if (Test-Path -LiteralPath $Output) {
    Remove-Item -LiteralPath $Output -Force
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::Open($Output, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($entry in $files.GetEnumerator()) {
        [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $archive,
            $entry.Value,
            $entry.Key,
            [System.IO.Compression.CompressionLevel]::Optimal
        ) | Out-Null
    }
}
finally {
    $archive.Dispose()
}

Get-Item -LiteralPath $Output | Select-Object FullName, Length
