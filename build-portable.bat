@echo off
chcp 65001 >nul
setlocal EnableExtensions
title rs-meetingrecorder-craft 便携版打包

rem 纯 Rust 程序:输出「双 exe + Nomo 阅读器 + 说明」的便携目录。
rem 目标目录可直接拷到任何 Windows 10/11 x64 机器上运行;
rem STT/说话人分离全本地(SenseVoice + ONNX),模型首次开会自动下载,便携包不带模型。

cd /d "%~dp0"
set "ROOT=%~dp0"
set "INPUT1=%ROOT%target\release\mrc-recorder.exe"
set "INPUT2=%ROOT%target\release\mrc-postprocess.exe"
set "OUTDIR=%ROOT%dist"
set "PKGDIR=%OUTDIR%\rs-meetingrecorder-portable"
set "ZIP=%OUTDIR%\rs-meetingrecorder-portable.zip"

rem 双击运行时 PATH 里可能没有 cargo
where cargo >nul 2>&1
if errorlevel 1 if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo ========================================
echo  1/3 编译 release 版
echo ========================================
cargo build --release
if errorlevel 1 (
    echo [错误] cargo build --release 失败
    goto :fail
)
if not exist "%INPUT1%" (
    echo [错误] 未找到编译产物: %INPUT1%
    goto :fail
)
if not exist "%INPUT2%" (
    echo [错误] 未找到编译产物: %INPUT2%
    goto :fail
)

echo.
echo ========================================
echo  2/3 组装便携目录
echo ========================================
if not exist "%OUTDIR%" mkdir "%OUTDIR%"
if exist "%PKGDIR%" rmdir /s /q "%PKGDIR%"
if not exist "%PKGDIR%" mkdir "%PKGDIR%"
copy /y "%INPUT1%" "%PKGDIR%\mrc-recorder.exe" >nul
copy /y "%INPUT2%" "%PKGDIR%\mrc-postprocess.exe" >nul
copy /y "%ROOT%config.example.toml" "%PKGDIR%\config.example.toml" >nul
rem 内置 Markdown 阅读器 Nomo(AGPL-3.0,原样分发,License 随目录自带)
if exist "%ROOT%thirdpart\Nomo\nomo.exe" (
    xcopy /e /i /y "%ROOT%thirdpart\Nomo" "%PKGDIR%\thirdpart\Nomo" >nul
) else (
    echo [提示] thirdpart\Nomo 不存在,跳过内置阅读器(面板「Nomo 查看」按钮将隐藏)
)
if errorlevel 1 (
    echo [错误] 复制文件失败
    goto :fail
)

rem 附一份简短说明(用 PowerShell 写,避免 bat 括号转义问题)
powershell -NoProfile -Command ^
  "$lines = @('rs-meetingrecorder-craft 便携版','','直接双击 mrc-recorder.exe 运行,不需要安装。','','文件说明:','  mrc-recorder.exe        主程序(不带参数=GUI;--start/--status=无头 CLI)','  mrc-postprocess.exe     后处理 CLI(list/run/name…)','  thirdpart\Nomo\         内置 Markdown 阅读器 Nomo(原样分发,AGPL-3.0,License 在目录内)','  (SenseVoice/说话人分离模型首次开会自动下载,全本地运行,音频不出本机)','','首次使用:','  1. 模型免手动下载:转写/分离模型首次开会自动拉取(hf-mirror)','  2. 运行 mrc-postprocess init-config 生成配置;[llm] 选 OpenAI 兼容 API,或本地 GGUF(设置里按显存一键下载推荐模型)','  3. F9 开始/结束会议;F8 截图归 pointtofixcraft(可在设置里安装)','  4. 散会后点「Nomo 查看」一屏看完纪要+截图(任何 Markdown 阅读器打开 minutes.md 亦可)','','数据位置: %APPDATA%\ltl-meetingrecord\(meetings.db + packages\ 会议包 + audio\ 录音)');" ^
  "[IO.File]::WriteAllLines('%PKGDIR%\使用说明.txt', $lines, (New-Object Text.UTF8Encoding $true))"

echo   便携目录: %PKGDIR%

echo.
echo ========================================
echo  3/3 压缩成 zip
echo ========================================
if exist "%ZIP%" del /f /q "%ZIP%"
powershell -NoProfile -Command "Compress-Archive -Path '%PKGDIR%\*' -DestinationPath '%ZIP%' -Force" >nul 2>&1
if exist "%ZIP%" (
    echo   已生成: %ZIP%
) else (
    echo   [提示] 压缩失败或系统没有 Compress-Archive,直接用目录即可: %PKGDIR%
)

echo.
echo ========================================
echo   打包成功
echo ========================================
echo   目录: %PKGDIR%
echo   zip : %ZIP%
exit /b 0

:fail
echo.
echo   打包失败
exit /b 1
