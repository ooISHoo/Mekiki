@echo off
setlocal EnableExtensions EnableDelayedExpansion

set "ROOT=%~dp0.."
pushd "%ROOT%" >nul || exit /b 1

set "EXIT_CODE=1"
set "BUNDLE=%~1"
if not defined BUNDLE set "BUNDLE=nsis"

if /I "%BUNDLE%"=="nsis" goto bundle_ok
if /I "%BUNDLE%"=="msi" goto bundle_ok
if /I "%BUNDLE%"=="all" goto bundle_ok
echo Usage: %~nx0 [nsis^|msi^|all]
goto cleanup

:bundle_ok
set "TAURI_BUNDLES=%BUNDLE%"
if /I "%BUNDLE%"=="all" set "TAURI_BUNDLES=nsis,msi"

set "NPM_EXE="
for /f "delims=" %%I in ('where npm.cmd 2^>nul') do if not defined NPM_EXE set "NPM_EXE=%%I"
if not defined NPM_EXE (
    echo ERROR: npm.cmd was not found in PATH.
    goto cleanup
)

set "CARGO_EXE="
for /f "delims=" %%I in ('where cargo.exe 2^>nul') do if not defined CARGO_EXE set "CARGO_EXE=%%I"
if not defined CARGO_EXE if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO_EXE=%USERPROFILE%\.cargo\bin\cargo.exe"
if not defined CARGO_EXE (
    echo ERROR: cargo.exe was not found in PATH or %%USERPROFILE%%\.cargo\bin.
    goto cleanup
)

for %%I in ("%CARGO_EXE%") do set "PATH=%%~dpI;%PATH%"

set "VERSION="
for /f "tokens=2 delims=:, " %%V in ('findstr /R /C:"\"version\"" "ide\src-tauri\tauri.conf.json"') do if not defined VERSION set "VERSION=%%~V"
if not defined VERSION (
    echo ERROR: Could not read the version from ide\src-tauri\tauri.conf.json.
    goto cleanup
)

set "TARGET_TRIPLE="
for /f "tokens=2" %%T in ('rustc -vV 2^>nul ^| findstr /B "host:"') do set "TARGET_TRIPLE=%%T"
if /I "%TARGET_TRIPLE%"=="x86_64-pc-windows-msvc" (
    set "ARCH=x64"
) else (
    echo ERROR: Unsupported Rust host target: %TARGET_TRIPLE%
    echo This installer batch currently supports x86_64-pc-windows-msvc only.
    goto cleanup
)

set "SIDECAR_DIR=ide\src-tauri\binaries"
set "SIDECAR=%SIDECAR_DIR%\mekiki-mcp-%TARGET_TRIPLE%.exe"
set "SIDECAR_CLI=%SIDECAR_DIR%\mekiki-%TARGET_TRIPLE%.exe"
set "OUTPUT_DIR=dist\installer\v%VERSION%"

echo.
echo Building Mekiki %VERSION% installer [%TAURI_BUNDLES%]...
echo Cargo: %CARGO_EXE%
echo Output: %OUTPUT_DIR%
echo.

if /I not "%MEKIKI_SKIP_TESTS%"=="1" (
    call "%NPM_EXE%" --prefix ide ci
    if errorlevel 1 goto cleanup

    call "%NPM_EXE%" --prefix ide test
    if errorlevel 1 goto cleanup

    "%CARGO_EXE%" test --workspace --release --features mekiki-ide/custom-protocol
    if errorlevel 1 goto cleanup
) else (
    echo MEKIKI_SKIP_TESTS=1: dependency install and tests are skipped.
)

"%CARGO_EXE%" build --release -p mekiki-mcp -p mekiki-scripting --bin mekiki-mcp --bin mekiki
if errorlevel 1 goto cleanup

rem Both extra binaries travel as Tauri sidecars and land next to mekiki-ide.exe.
rem The installer does not touch PATH; users add the install directory themselves.
if not exist "%SIDECAR_DIR%" mkdir "%SIDECAR_DIR%"
if errorlevel 1 goto cleanup
copy /Y "target\release\mekiki-mcp.exe" "%SIDECAR%" >nul
if errorlevel 1 goto cleanup
copy /Y "target\release\mekiki.exe" "%SIDECAR_CLI%" >nul
if errorlevel 1 goto cleanup

pushd ide >nul
call "%NPM_EXE%" run tauri -- build --ci --config src-tauri/tauri.bundle.conf.json --bundles %TAURI_BUNDLES% --features custom-protocol
set "TAURI_EXIT=!ERRORLEVEL!"
popd >nul
if not "%TAURI_EXIT%"=="0" goto cleanup

if not exist "%OUTPUT_DIR%" mkdir "%OUTPUT_DIR%"
if errorlevel 1 goto cleanup

set "FOUND_NSIS=0"
set "FOUND_MSI=0"

if /I "%BUNDLE%"=="nsis" call :collect_nsis
if /I "%BUNDLE%"=="msi" call :collect_msi
if /I "%BUNDLE%"=="all" (
    call :collect_nsis
    call :collect_msi
)

if /I "%BUNDLE%"=="nsis" if "!FOUND_NSIS!"=="0" (
    echo ERROR: The NSIS installer was not found.
    goto cleanup
)
if /I "%BUNDLE%"=="msi" if "!FOUND_MSI!"=="0" (
    echo ERROR: The MSI installer was not found.
    goto cleanup
)
if /I "%BUNDLE%"=="all" if not "!FOUND_NSIS!!FOUND_MSI!"=="11" (
    echo ERROR: One or more installer artifacts were not found.
    goto cleanup
)

powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; $out=[IO.Path]::GetFullPath('%OUTPUT_DIR%'); $hash=Join-Path $out 'SHA256SUMS-%VERSION%.txt'; $sha=[Security.Cryptography.SHA256]::Create(); $lines=@(Get-ChildItem -LiteralPath $out -File | Where-Object Name -ne ([IO.Path]::GetFileName($hash)) | Sort-Object Name | ForEach-Object { $stream=[IO.File]::OpenRead($_.FullName); try { $bytes=$sha.ComputeHash($stream) } finally { $stream.Dispose() }; '{0}  {1}' -f ([BitConverter]::ToString($bytes).Replace('-','').ToLowerInvariant()), $_.Name }); [IO.File]::WriteAllLines($hash, $lines, [Text.Encoding]::ASCII); $sha.Dispose()"
if errorlevel 1 goto cleanup

echo.
echo Installer build completed:
for %%F in ("%OUTPUT_DIR%\*") do echo   %%~fF
set "EXIT_CODE=0"
goto cleanup

:collect_nsis
for %%F in ("target\release\bundle\nsis\*_%VERSION%_*setup.exe") do if exist "%%~fF" (
    copy /Y "%%~fF" "%OUTPUT_DIR%\Mekiki-%VERSION%-windows-%ARCH%-setup.exe" >nul
    set "FOUND_NSIS=1"
)
exit /b 0

:collect_msi
for %%F in ("target\release\bundle\msi\*_%VERSION%_*.msi") do if exist "%%~fF" (
    copy /Y "%%~fF" "%OUTPUT_DIR%\Mekiki-%VERSION%-windows-%ARCH%.msi" >nul
    set "FOUND_MSI=1"
)
exit /b 0

:cleanup
if defined SIDECAR if exist "%SIDECAR%" del /Q "%SIDECAR%" >nul 2>&1
if defined SIDECAR_CLI if exist "%SIDECAR_CLI%" del /Q "%SIDECAR_CLI%" >nul 2>&1
popd >nul
exit /b %EXIT_CODE%
