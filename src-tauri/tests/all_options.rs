//! 逐个测试 configure() 中的选项，找出在 mpv_initialize 之前设置会失败的项。

use std::ffi::CString;

#[test]
fn test_all_options() {
    let dll = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("libmpv-2.dll");
    if !dll.exists() {
        eprintln!("libmpv-2.dll 不存在，跳过");
        return;
    }

    unsafe {
        let lib = libloading::Library::new(&dll).expect("加载 dll 失败");
        let create: libloading::Symbol<unsafe extern "C" fn() -> *mut std::ffi::c_void> =
            lib.get(b"mpv_create").expect("找不到 mpv_create");
        let init: libloading::Symbol<unsafe extern "C" fn(*mut std::ffi::c_void) -> i32> =
            lib.get(b"mpv_initialize").expect("找不到 mpv_initialize");
        let set_opt: libloading::Symbol<
            unsafe extern "C" fn(*mut std::ffi::c_void, *const i8, *const i8) -> i32,
        > = lib
            .get(b"mpv_set_option_string")
            .expect("找不到 mpv_set_option_string");
        let destroy: libloading::Symbol<unsafe extern "C" fn(*mut std::ffi::c_void)> =
            lib.get(b"mpv_terminate_destroy").expect("找不到 destroy");

        // 逐个测试 vo + gpu-context 组合
        let combos: Vec<(&str, &str)> = vec![
            ("gpu", "d3d11"),
            ("gpu", "angle"),
            ("opengl", "angle"),
            ("gpu-next", "d3d11"),
        ];

        for (vo_name, ctx_name) in combos {
            let ctx = create();
            assert!(!ctx.is_null());

            let c_vo = CString::new("vo").unwrap();
            let c_vo_val = CString::new(vo_name).unwrap();
            let code_vo = set_opt(ctx, c_vo.as_ptr(), c_vo_val.as_ptr());

            let c_ctx = CString::new("gpu-context").unwrap();
            let c_ctx_val = CString::new(ctx_name).unwrap();
            let code_ctx = set_opt(ctx, c_ctx.as_ptr(), c_ctx_val.as_ptr());

            let code_init = init(ctx);
            let status = if code_vo == 0 && code_ctx == 0 && code_init == 0 { "OK" } else { "FAIL" };
            eprintln!(
                "{status:4} vo={vo_name} + gpu-context={ctx_name}: set_vo={code_vo} set_ctx={code_ctx} init={code_init}"
            );

            destroy(ctx);
        }
    }
}
