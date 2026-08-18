# gyroflow_ffi

LutFTP（iOS 相机联机 + LUT 调色 App）的防抖桥接层（Rust crate）。

本 crate 将 [`gyroflow_core`](https://github.com/gyroflow/gyroflow/tree/master/src/core)
（`StabilizationManager`）的必要 API 封装为 C ABI（`#![no_mangle] pub extern "C"`），
供 Swift 侧通过 `include/gyroflow_bridge.h` 调用。

## 功能

- `gf_engine_create` / `gf_engine_destroy`：引擎生命周期
- `gf_engine_load_video`：载入视频并解析内嵌陀螺仪元数据（索尼/GoPro 等）
- `gf_engine_set_level`：防抖档位（关/低/中/高）+ 平滑/裁切/地平线锁/果冻校正参数
- `gf_engine_process_frame`：逐帧 BGRA 防抖处理（旋转稳定 + 自适应裁切）

## 构建（iOS）

前置：`rustup` + iOS 目标：

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
```

在仓库根目录执行（也可用 `--offline` 使用本地缓存）：

```bash
./build_gyroflow_core.sh
```

产物：`LutFtp/Gyroflow/GyroflowCore.xcframework`（含本 crate 与 gyroflow-core 静态库）。

> 依赖说明：`Cargo.toml` 的 `[patch]` 段将 `telemetry-parser`/`mp4parse`
> 指向本地 `deps/`（网络不可达时的构建配置，不改变算法/业务代码）。
> 有网络时删除该段即可恢复上游依赖。

## 关键实现说明

- 参数档位（`SMOOTHNESS`/`ZOOM_WINDOW`/`HORIZON_LOCK`）映射豆包 AI 3 档预设：
  低=电影感少裁切 / 中=日常平衡 / 高=强防抖。
- `adaptive_zoom_window` 单位是**秒**（非百分比），窗口越大 fov 平滑越强。
- `gf_engine_load_video` / `gf_engine_set_level` 末尾调用阻塞式
  `recompute_blocking()`：不依赖 UI 回调的异步重算路径，保证
  smoothness / fovs 真正计算并同步到渲染侧。
- 地平线锁只锁滚转（`lock_pitch=false`）：避免俯仰强制拉平导致 fov>1（画面缩小黑边）。
- 输入尺寸为视频**原始方向**（`videoTrack.naturalSize`），旋转由 Swift 侧后置处理。
- 镜头畸变校正默认关闭（xcframework 未打包 gyroflow lens profile 数据库）。

## 合规（GPLv3）

- 本 crate 源码按 GPLv3 发布（见 `LICENSE`，含 App Store 附加许可条款）。
- `gyroflow-core` 源码**未做任何修改**（仅作为依赖链接）。
- LutFTP 业务 Swift 代码通过 C ABI 调用本 crate，非衍生作品，保持闭源。

## 源码获取

- gyroflow：<https://github.com/gyroflow/gyroflow>
- 本 crate：<https://github.com/alonefeizai/gyroflow-ffi>
