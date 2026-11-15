@echo off
setlocal EnableExtensions DisableDelayedExpansion
rem .cmd/build.cmd
rem Zero-downtime release build to <target>\release\rust-agents-proxy.exe, then Authenticode sign.
rem A running exe cannot be deleted or overwritten on Windows, but it can be renamed:
rem the current binary moves aside as rust-agents-proxy.exe.stale-<n>, a running serve keeps
rem executing the renamed image, and cargo links a fresh binary onto the original path.
rem Restart serve afterwards (admin drain or Task Scheduler) to pick up the new build.

pushd "%~dp0.." || goto :err

where cargo >nul 2>&1 || (
    echo cargo was not found in PATH.
    goto :err
)

if defined CARGO_TARGET_DIR (set "TARGET_DIR=%CARGO_TARGET_DIR%") else set "TARGET_DIR=%CD%\target"
set "RELEASE_DIR=%TARGET_DIR%\release"
set "EXE_PATH=%RELEASE_DIR%\rust-agents-proxy.exe"
echo Release binary: "%EXE_PATH%"

rem Clear stale images from earlier builds; ones still executing fail silently and stay.
del /f /q "%RELEASE_DIR%\rust-agents-proxy.exe.stale-*" >nul 2>&1
del /f /q "%RELEASE_DIR%\deps\rust_agents_proxy-*.exe.stale-*" >nul 2>&1

rem The release exe is a hard link of deps\rust_agents_proxy-<hash>.exe, and a running
rem image locks every name of that file, so the deps link output moves aside as well.
for %%F in ("%RELEASE_DIR%\deps\rust_agents_proxy-*.exe") do ren "%%~fF" "%%~nxF.stale-%RANDOM%%RANDOM%" >nul 2>&1

if exist "%EXE_PATH%" (
    ren "%EXE_PATH%" "rust-agents-proxy.exe.stale-%RANDOM%%RANDOM%" || (
        echo Failed to move the current binary aside: "%EXE_PATH%"
        goto :err
    )
)

echo === cargo build --release --offline ===
cargo build --release --offline
if errorlevel 1 goto :err

if not exist "%EXE_PATH%" (
    echo Build completed without the expected binary: "%EXE_PATH%"
    goto :err
)

for %%I in ("%EXE_PATH%") do echo Built: "%%~fI" ^(%%~zI bytes^)

rem A signing failure keeps the unsigned build usable and only warns.
set "PS_EXE=powershell"
where pwsh >nul 2>&1 && set "PS_EXE=pwsh"
echo === sign ===
"%PS_EXE%" -NoProfile -NonInteractive -File "%~dp0sign.ps1" -ExePath "%EXE_PATH%"
if errorlevel 1 echo WARNING: signing failed, the binary stays unsigned.

popd
echo Done.
endlocal
exit /b 0

:err
echo FAILED
popd
endlocal
exit /b 1
