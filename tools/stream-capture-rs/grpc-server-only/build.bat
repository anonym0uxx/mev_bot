@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" %1
cd /d D:\repos\mev_bot\tools\stream-capture-rs\grpc-server-only
cargo build --release
echo BUILD_EXIT_CODE=%errorlevel%
