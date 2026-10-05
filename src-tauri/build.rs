//! libmpv 需要的 DLL 放到 exe 能找到的地方，并把图标 / 清单 / 版本信息写进 exe 资源段。

use std::path::{Path, PathBuf};

fn main() {
    embed_resources();
    copy_libmpv();

    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=libmpv-2.dll");
    // 版本号是从 Cargo.toml 读进资源段的，改了 Cargo.toml 必须重编资源
    println!("cargo:rerun-if-changed=Cargo.toml");
}

/// 把图标、DPI 清单与版本信息嵌进 exe。
///
/// 少了这一步 exe 会用 Windows 默认图标，文件属性里也没有版本号。清单里的
/// DPI 感知声明尤其关键：它由系统在进程创建时加载，比 `app` 里显式调用
/// `SetProcessDpiAwarenessContext` 更早，进程一旦创建窗口就再也改不了 DPI 模式。
///
/// 这里刻意不让它中断构建：没装 Windows SDK（找不到 rc.exe）时只告警，产物
/// 仍然可用，只是没有图标。
fn embed_resources() {
    let rc_path = Path::new(&env("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("app.rc");
    if !rc_path.exists() {
        println!("cargo:warning=缺少 assets/app.rc，跳过资源嵌入");
        return;
    }

    // 元组的第一项是迭代器、元素类型才是 `AsRef<OsStr>`，所以这里传的是
    // 拼好的 `NAME=VALUE` 字符串数组（等价于 rc.exe 的 /d 参数）。
    //
    // 两种宏的引号要求不一样，这是 rc.exe 预处理器的坑：
    // * `FILEVERSION` 要的是**裸**数字列表 `0,2,0,0`，加了引号就解析不了
    // * `VALUE` 那一行要的是**带引号**的字符串。写成裸的 `0.2.0` 会被当成
    //   一个非法数字字面量，字段值悄悄变成空——文件属性里版本号整片消失，
    //   而且不报任何错
    let macros = [
        format!("VV_FILEVERSION={}", file_version_numbers()),
        format!("VV_VERSION=\"{}\"", version_string()),
    ];
    // 直接用元组结构体构造而不是 `from`：泛型四元组上同时存在
    // `From<Self> for Self` 与自定义的 `From<ParamsMacros>`，
    // 编译器会优先选中前者，于是报「参数个数不对」
    let params = embed_resource::ParamsMacrosAndIncludeDirs(macros, embed_resource::NONE);

    // manifest_optional：清单是可选资源。没有它 exe 依然能跑，
    // 只是少了 DPI 声明的双保险。
    if let Err(e) = embed_resource::compile(rc_path, params).manifest_optional() {
        println!("cargo:warning=资源编译失败（未安装 Windows SDK?）: {e}");
    }
}

/// `VERSIONINFO.FILEVERSION` 要求的四个数字，格式 `major,minor,patch,build`。
///
/// 少于四段就补 0，多于四段就截断。预发布后缀（`-rc1`、`+build`）在这一层
/// 已经被剥掉了，字符串版本由 `version_string()` 单独处理。
fn file_version_numbers() -> String {
    let mut parts = [0u32; 4];
    for (i, seg) in version_string().split('.').take(4).enumerate() {
        parts[i] = seg.parse().unwrap_or(0);
    }
    parts
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// 文件属性 / 安装包要显示的字符串版本号，只保留 `x.y.z` 的数字段。
///
/// 语义化版本里的预发布后缀没法原样塞进 rc 预处理，这里直接丢掉：
/// 「0.2.0-rc1 的 FileVersion 显示 0.2.0」是可接受的，
/// 「显示 0.2.0-rc1」会让 rc.exe 解析失败。
fn version_string() -> String {
    let core = env("CARGO_PKG_VERSION")
        .split(['-', '+'])
        .next()
        .unwrap_or("0.0.0")
        .to_string();
    // 保证至少三段，`version_string` 的调用方才有稳定的 x.y.z
    let segs = core.split('.').count();
    if segs >= 3 {
        core
    } else {
        format!("{core}.{}", "0".repeat(3 - segs))
    }
}

/// libmpv 是运行时动态加载的，构建期需要把它放到 exe 找得到的地方。
///
/// 开发期 exe 在 `target/<profile>/video-view.exe`，exe 同级就是
/// `resolve_library_path()` 的第一搜索位；打包时由 NSIS 脚本负责投放。
/// 所以按 profile 复制，而不是复制到 OUT_DIR（那里 exe 拿不到）。
fn copy_libmpv() {
    let Some(dll) = locate_dll() else {
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
    PathBuf::from(env("OUT_DIR"))
        .ancestors()
        // out -> <pkg-hash> -> build -> <profile>
        .nth(3)
        .map(PathBuf::from)
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
