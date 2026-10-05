// release 构建不弹控制台窗口。
//
// 调试构建保留控制台：panic 的回溯信息会打在标准错误上，没有它就只能靠猜。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    video_view_lib::run();
}
