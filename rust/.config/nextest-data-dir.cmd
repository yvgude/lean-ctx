@echo off
rem SPDX-License-Identifier: Apache-2.0
rem nextest wrapper script for Windows: the counterpart of nextest-data-dir.sh.
rem Gives every test process its own empty LEAN_CTX_DATA_DIR (leaf ".lean-ctx",
rem so is_data_dir_collision() still sees parent\.lean-ctx == data_dir), runs
rem the test, removes the directory and exits with the test's exit code.
rem
rem mkdir fails on an existing directory, so a name taken by a parallel test
rem (cmd seeds RANDOM per second) is retried with the next one. No labels or
rem goto, so the script works with LF and CRLF line endings alike.
setlocal EnableExtensions EnableDelayedExpansion
set "LCNX_BASE=%TEMP%"
if not defined LCNX_BASE set "LCNX_BASE=%TMP%"
set "LCNX_ROOT="
for /l %%i in (1,1,64) do (
  if not defined LCNX_ROOT (
    set "LCNX_TRY=!LCNX_BASE!\lcnx.%NEXTEST_RUN_ID%.!RANDOM!!RANDOM!.%%i"
    mkdir "!LCNX_TRY!" 2>nul && set "LCNX_ROOT=!LCNX_TRY!"
  )
)
if not defined LCNX_ROOT (
  echo nextest-data-dir: cannot create a temporary directory under "%LCNX_BASE%" 1>&2
  exit /b 1
)
mkdir "%LCNX_ROOT%\.lean-ctx" || (
  rmdir /s /q "%LCNX_ROOT%" 2>nul
  exit /b 1
)
set "LEAN_CTX_DATA_DIR=%LCNX_ROOT%\.lean-ctx"
rem The test command line must not go through delayed expansion ("!" in args).
setlocal DisableDelayedExpansion
%*
set "LCNX_RC=%ERRORLEVEL%"
rmdir /s /q "%LCNX_ROOT%" 2>nul
exit /b %LCNX_RC%
