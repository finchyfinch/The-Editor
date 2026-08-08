@echo off
setlocal enabledelayedexpansion
rem ---------------------------------------------------------------------------
rem Build a Windows release of The Editor and stage it for distribution.
rem
rem The whole target\release folder is emphatically NOT what gets shipped -- it
rem is well over a gigabyte of intermediate objects, and exactly one file in it
rem is the program. Everything The Editor needs at runtime is compiled into the
rem executable: the tree-sitter grammars, their highlight queries, the file
rem templates, the themes and the icon. There is no assets folder to copy, no
rem DLL to sit beside it, and no runtime to install first -- the C runtime is
rem linked statically (see .cargo\config.toml), so this runs on a machine that
rem has never had Visual Studio near it.
rem
rem Produces, in dist\:
rem     the-editor-<version>-windows-x64\      the folder a user unzips
rem     the-editor-<version>-windows-x64.zip   what you actually send them
rem     the-editor-<version>-windows-x64.zip.sha256
rem
rem The checksum matters more than usual here: the binary is deliberately
rem unsigned (PLAN.md decision D10), so a published hash is the only way for
rem someone to check they received what was sent.
rem ---------------------------------------------------------------------------

cd /d "%~dp0\.."

rem The version comes from the workspace manifest, so there is one place to
rem change it and no chance of the zip being named after the wrong release.
for /f "tokens=2 delims== " %%v in ('findstr /b /c:"version = " Cargo.toml') do (
    if not defined VERSION set VERSION=%%~v
)
if not defined VERSION (
    echo Could not read the version from Cargo.toml.
    exit /b 1
)

set NAME=the-editor-%VERSION%-windows-x64
set STAGE=dist\%NAME%

echo Building The Editor %VERSION% ...
cargo build --release --package the-editor
if errorlevel 1 (
    echo.
    echo Build failed. Nothing has been staged.
    exit /b 1
)

rem Tests are not optional for something about to be handed to someone else.
echo Running the test suite ...
cargo test --workspace --quiet
if errorlevel 1 (
    echo.
    echo Tests failed. Refusing to package a build that does not pass them.
    exit /b 1
)

if exist "%STAGE%" rmdir /s /q "%STAGE%"
mkdir "%STAGE%" 2>nul

copy /y "target\release\the-editor.exe" "%STAGE%\" >nul
copy /y "LICENSE" "%STAGE%\LICENSE.txt" >nul
copy /y "CHANGELOG.md" "%STAGE%\CHANGELOG.txt" >nul
if exist "README.md" copy /y "README.md" "%STAGE%\README.txt" >nul

rem Windows will warn about an unsigned executable downloaded from the web.
rem Saying so up front, with the way round it, is better than the user deciding
rem the program is malware.
> "%STAGE%\FIRST-RUN.txt" (
    echo The Editor %VERSION% - portable build for Windows x64
    echo.
    echo Run the-editor.exe. There is nothing to install: everything the
    echo program needs is inside that one file, and it writes its settings to
    echo your user profile the first time it starts.
    echo.
    echo Windows may show "Windows protected your PC" the first time, because
    echo this build is not code-signed. Click "More info", then "Run anyway".
    echo Check the published SHA-256 of the zip first if you did not build it
    echo yourself.
    echo.
    echo Optional, and detected automatically if present:
    echo   pip install ruff           linting
    echo   pip install basedpyright   types, completion, go to definition
    echo   pip install debugpy        the debugger
    echo   rustup component add rust-analyzer
    echo.
    echo Help - Check Toolchains lists what was found.
)

echo Packaging ...
if exist "dist\%NAME%.zip" del /q "dist\%NAME%.zip"
powershell -NoProfile -Command ^
    "Compress-Archive -Path '%STAGE%' -DestinationPath 'dist\%NAME%.zip' -Force"
if errorlevel 1 (
    echo Could not create the zip.
    exit /b 1
)

powershell -NoProfile -Command ^
    "(Get-FileHash 'dist\%NAME%.zip' -Algorithm SHA256).Hash.ToLower() + '  %NAME%.zip'" ^
    > "dist\%NAME%.zip.sha256"

echo.
echo Done.
for %%f in ("dist\%NAME%.zip") do echo   dist\%NAME%.zip  (%%~zf bytes)
type "dist\%NAME%.zip.sha256"
endlocal
