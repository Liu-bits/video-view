use std::path::{Path, PathBuf};

/// libmpv 是运行时动态加载的，所以构建期需要把它放到 exe 能找到的地方。
///
/// 开发期 exe 在 `target/<profile>/video-view.exe`，exe 同级就是
/// `resolve_library_path()` 的第一搜索位；打包后由 Tauri 的 resources
/// 机制放到安装目录。因此这里按 profile 复制而不是复制到 OUT_DIR。
fn main() {
    tauri_build::build();

    println!("cargo:rerun-if-changed=libmpv-2.dll");

    let Some(dll) = locate_dll() else {
        // 不直接 panic：正式打包由 Tauri 的 resources 负责带上 DLL。
        println!("cargo:warning=未找到 libmpv-2.dll，跳过开发期复制");
        return;
    };

    let Some(profile_dir) = profile_dir() else {
        println!("cargo:warning=无法推导 profile 目录，跳过开发期复制");
        return;
    };

    let target = profile_dir.join("libmpv-2.dll");
    if needs_copy(&dll, &target) {
        if let Err(e) = std::fs::copy(&dll, &target) {
            println!(
                "cargo:warning=复制 {} 到 {} 失败: {e}",
                dll.display(),
                target.display()
            );
        }
    }
}

/// DLL 的查找顺序：先看 src-tauri 下（项目自带），再看 vendor 目录。
fn locate_dll() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let mut candidates = vec![manifest_dir.join("libmpv-2.dll")];
    if let Some(parent) = manifest_dir.parent() {
        candidates.push(parent.join("vendor").join("libmpv-2.dll"));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// 从 `OUT_DIR`（形如 `target/<profile>/build/<pkg>-<hash>/out`）反推 profile 目录。
fn profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(env("OUT_DIR"));
    out_dir
        .ancestors()
        // out -> <pkg-hash> -> build -> <profile>
        .nth(3)
        .map(Path::to_path_buf)
}

/// 只在源文件更新时复制，避免每次编译都搬 100+MB。
fn needs_copy(src: &Path, dst: &Path) -> bool {
    match (std::fs::metadata(src), std::fs::metadata(dst)) {
        (Ok(s), Ok(d)) => {
            let newer = s
                .modified()
                .and_then(|sm| d.modified().map(|dm| sm > dm))
                .unwrap_or(false);
            newer || s.len() != d.len()
        }
        _ => true,
    }
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}
