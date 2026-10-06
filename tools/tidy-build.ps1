# ---------------------------------------------------------------------------
# Throw away build output that is only ever needed again by a rebuild.
#
# Cargo never garbage-collects target\ (see .cargo\config.toml): every change
# to a flag, a feature or a dependency leaves the old artefacts behind under a
# stale hash. Left alone the folder climbed to 13.8 GB, and again to 7.8 GB.
# This runs at the end of tools\make-release.bat and of the pre-push hook, so
# nobody has to remember to do it.
#
#   dist\<name>\          the unzipped staging folder of a release. The zip
#                         beside it holds exactly the same files, so it goes
#                         once the zip exists. The zips and their .sha256 are
#                         kept: they are what was published.
#   target\release\       1.8 GB of intermediates that only a release build
#                         reads. Only with -Release, i.e. after the release has
#                         been packaged; the next release rebuilds from scratch,
#                         which is what a release should do anyway.
#   target\debug\         kept while it is under the limit, because it is what
#                         makes an everyday rebuild take seconds. Over the
#                         limit it is mostly dead hashes, so the dev profile is
#                         cleaned outright and settles back to ~4 GB on the
#                         next build (~100 s).
#
# Never fails the caller: a tidy that cannot finish (a running the-editor.exe
# holds its file open, say) is not a reason to stop a push or a release.
# ---------------------------------------------------------------------------
param(
    [switch]$Release,
    [int]$DebugLimitGB = 8
)

$ErrorActionPreference = 'Continue'
Set-Location (Join-Path $PSScriptRoot '..')

function SizeGB($path) {
    if (-not (Test-Path $path)) { return 0 }
    $bytes = (Get-ChildItem $path -Recurse -Force -File -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
    return [math]::Round($bytes / 1GB, 1)
}

if (Test-Path dist) {
    Get-ChildItem dist -Directory | Where-Object { Test-Path "dist\$($_.Name).zip" } | ForEach-Object {
        Remove-Item $_.FullName -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "tidy: removed dist\$($_.Name) (the zip has it)"
    }
}

if ($Release -and (Test-Path target\release)) {
    $size = SizeGB target\release
    cargo clean --release --quiet
    Write-Host "tidy: removed target\release ($size GB)"
}

$size = SizeGB target\debug
if ($size -gt $DebugLimitGB) {
    cargo clean --profile dev --quiet
    Write-Host "tidy: target\debug had reached $size GB (limit $DebugLimitGB GB); cleaned it"
} elseif ($size -gt 0) {
    Write-Host "tidy: target\debug is $size GB (limit $DebugLimitGB GB); kept"
}

exit 0
