// SPDX-License-Identifier: GPL-3.0-or-later
// gyroflow_ffi — LutFTP 防抖桥接层（GPLv3，见项目 GPL 合规说明）。
// 仅封装 gyroflow_core::StabilizationManager 的必要 API 为 C ABI，
// 不修改 gyroflow-core 源码。Swift 侧通过 include/gyroflow_bridge.h 调用。

use std::ffi::{c_char, c_int};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use gyroflow_core::gyro_source::FileLoadOptions;
use gyroflow_core::gpu::{BufferDescription, BufferSource, Buffers};
use gyroflow_core::stabilization::BGRA8;
use gyroflow_core::stabilization_params::ReadoutDirection;
use gyroflow_core::StabilizationManager;

/// 防抖级别 → smoothness（平滑时间常数，单位秒；0 位未用）。
/// 豆包AI 3 档预设：低=电影感少裁切 / 中=日常平衡 / 高=强防抖。
const SMOOTHNESS: [f64; 4] = [0.0, 0.55, 0.70, 0.85];
/// 防抖级别 → adaptive_zoom_window（单位：**秒**，不是百分比！core 默认 4.0s）。
/// 窗口越大 → fov 平滑越强、裁切跟随越缓。旧值 0.05~0.12 把秒当百分比用，
/// 等效 1~4 帧窗口，导致动态裁切（fov 曲线）完全失效。
const ZOOM_WINDOW: [f64; 4] = [0.0, 2.5, 3.5, 5.0];
/// 防抖级别 → horizon lock 强度（百分比 0~100，core 默认 100）。
const HORIZON_LOCK: [f64; 4] = [0.0, 60.0, 85.0, 90.0];

#[repr(C)]
pub struct GFEngine {
    inner: StabilizationManager,
}

#[no_mangle]
pub extern "C" fn gf_engine_create() -> *mut GFEngine {
    let engine = Box::new(GFEngine {
        inner: StabilizationManager::default(),
    });
    Box::into_raw(engine)
}

#[no_mangle]
pub unsafe extern "C" fn gf_engine_destroy(e: *mut GFEngine) {
    if !e.is_null() {
        drop(Box::from_raw(e));
    }
}

/// 载入视频并解析内嵌陀螺仪元数据。
/// width/height 必须传**原始方向**像素尺寸（AVAssetTrack.naturalSize，不含旋转）：
/// 防抖在原始方向像素上处理，旋转由 Swift 侧 applyLUTsToVideo 后置完成。
/// 返回 0=成功；非 0=失败码。has_motion 回填是否有可用陀螺仪数据。
#[no_mangle]
pub unsafe extern "C" fn gf_engine_load_video(
    e: *mut GFEngine,
    url: *const c_char,
    duration_ms: f64,
    fps: f64,
    frame_count: c_int,
    width: c_int,
    height: c_int,
    has_motion: *mut c_int,
) -> c_int {
    if e.is_null() || url.is_null() {
        return -1;
    }
    let engine = &mut (*e).inner;

    let path = match std::ffi::CStr::from_ptr(url).to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => return -2,
    };

    let w = width.max(1) as usize;
    let h = height.max(1) as usize;

    engine.init_from_video_data(
        duration_ms.max(0.0),
        fps.max(0.0),
        frame_count.max(0) as usize,
        (w, h),
    );

    // 输出尺寸 = 输入尺寸：裁切由自适应 zoom 内部产生，输出分辨率不变
    engine.set_render_params((w, h), (w, h));

    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return -3,
    };
    let filesize = std::fs::metadata(&path)
        .map(|m| m.len() as usize)
        .unwrap_or(0);

    let cancel = Arc::new(AtomicBool::new(false));
    let options = FileLoadOptions {
        sample_index: None,
        project_version: 0,
    };

    let result = engine.load_gyro_data(
        &mut file,
        filesize,
        &path,
        true,
        &options,
        |_progress: f64| {},
        cancel,
    );

    // ── fov：保持 1.0（不预放大），裁切完全交给 adaptive zoom 自动计算 ──
    // 旧实现按 25mm 参考焦距换算 fov_scale（75mm → 0.6）会强制 1.67x 硬放大：
    // 长焦素材被无谓裁切、广角素材又无裁切余量。gyroflow 的 fov_iterative +
    // adaptive zoom 会基于实际抖动逐帧给出 fov，无需预放大。
    // 注意：读取焦距的 gyro 读锁必须在此处（后续任何 gyro.write() 之前）释放。
    let focal = {
        let gyro = engine.gyro.read();
        let md = gyro.file_metadata.read();
        md.lens_params.values().next().and_then(|p| p.focal_length.map(|f| f as f64))
    };
    println!("[gyroflow] EXIF 焦距: {:?}mm（fov=1.0，裁切由 adaptive zoom 自动计算）", focal);
    engine.set_fov(1.0);

    // 关键：阻塞式全量重算（smoothness → adaptive zoom → undistortion 并同步到
    // stabilization.compute_params）。旧实现用 override_video_fps 惰性触发，走的是
    // 依赖 UI 回调的异步 recompute_threaded 路径（FFI 无回调），导致 fovs /
    // smoothed_quaternions 从未被计算：fov 恒等于 fov_scale、min_fov 恒为 1.0，
    // 旋转稳定与动态裁切实际均未生效（日志：fov=0.6000 min_fov=1.0000 恒定）。
    engine.recompute_blocking();

    // ── 诊断日志：定位 fov>1（画面被迫缩小→黑边/全黑）的来源 ──
    // 若 camera_matrix fx/fy 数量级异常 → 索尼 lens_params 单位换算问题；
    // 若 distortion_coeffs 非零且数量级大 → 镜头 profile 系数不匹配；
    // 若 mesh_correction 非空 → 索尼网格校正数据问题。
    {
        let params = engine.params.read();
        let lens = engine.lens.read();
        let gyro = engine.gyro.read();
        let md = gyro.file_metadata.read();
        let f = lens.get_camera_matrix((params.size.0, params.size.1), false);
        let k = lens.get_distortion_coeffs();
        let (fovs_min, fovs_max) = params.fovs.iter().copied().fold((f64::MAX, f64::MIN), |(mn, mx), v| (mn.min(v), mx.max(v)));
        let (mf_min, mf_max) = params.minimal_fovs.iter().copied().fold((f64::MAX, f64::MIN), |(mn, mx), v| (mn.min(v), mx.max(v)));
        println!("[gyroflow] 诊断 lens: focal={:?} distortion_model={:?} k={:?}",
                 lens.focal_length, lens.distortion_model, k);
        println!("[gyroflow] 诊断 camera_matrix: fx={:.2} fy={:.2} cx={:.2} cy={:.2}",
                 f[(0, 0)], f[(1, 1)], f[(0, 2)], f[(1, 2)]);
        println!("[gyroflow] 诊断 元数据: mesh_correction={} 组, lens_params={} 条, distortion_coeffs(内置)={} 个",
                 md.mesh_correction.len(), md.lens_params.len(), lens.fisheye_params.distortion_coeffs.len());
        println!("[gyroflow] 诊断 fovs: {} 帧 [min={:.4} max={:.4}] | minimal_fovs [min={:.4} max={:.4}]",
                 params.fovs.len(), fovs_min, fovs_max, mf_min, mf_max);
    }

    if !has_motion.is_null() {
        *has_motion = if engine.gyro.read().has_motion() { 1 } else { 0 };
    }

    match result {
        Ok(()) => 0,
        Err(_) => -4,
    }
}

/// 设置防抖级别（1=低 2=中 3=高）与变速倍率（时间轴映射用）。
#[no_mangle]
pub unsafe extern "C" fn gf_engine_set_level(
    e: *mut GFEngine,
    level: c_int,
    video_speed: f64,
) {
    if e.is_null() {
        return;
    }
    let engine = &(*e).inner;
    let lvl = level.clamp(1, 3) as usize;

    engine.set_smoothing_param("smoothness", SMOOTHNESS[lvl]);
    engine.set_adaptive_zoom(ZOOM_WINDOW[lvl]);
    engine.set_zooming_method(1); // 1 = adaptive
    engine.set_video_speed(video_speed.max(0.01), false, false, false);
    // 地平线水平校正：只锁滚转(roll)，不锁俯仰(pitch)。
    // lock_pitch=true（旧实现）会对俯仰做强制拉平，产生大角度 counter-rotation，
    // 使 fov_iterative 算出 fov>1（画面被迫缩小 → 黑边/全黑，见诊断日志 fov=1.47）。
    // 仅锁 roll 是 gyroflow UI 默认行为：保留手持俯仰拍摄意图，同时校正倾斜。
    engine.set_horizon_lock(HORIZON_LOCK[lvl], 0.0, false, 0.0, false, 5.0, 500.0, 1.0, f64::INFINITY);
    engine.set_horizon_lock_integration_method(1); // 1 = VQF
    // 滚动快门果冻校正兜底：元数据缺失时按索尼 CMOS 惯例（TopToBottom, 12ms）；
    // 元数据已有精确值（load_gyro_data 自动读取）则保持不动。
    let readout_time = engine.params.read().frame_readout_time;
    if readout_time <= 0.0 {
        engine.set_frame_readout_direction(ReadoutDirection::TopToBottom);
        engine.set_frame_readout_time(12.0);
    }
    // 最大放大 1.4x：给自适应裁切足够余量，同时防止把画面切得太小
    engine.set_max_zoom(140.0, 5);
    // 关闭镜头畸变校正：LutFTP 的 xcframework 未打包 gyroflow lens profile 数据库，
    // 索尼元数据内嵌的镜头 profile 与 75mm 长焦不匹配时校正结果不可靠；
    // 长焦镜头畸变微小，直接关闭校正（保留防抖旋转 + 滚动快门校正）。
    engine.set_lens_correction_amount(0.0);

    // 阻塞式全量重算（见 gf_engine_load_video 说明）：不依赖异步回调，保证
    // smoothness / fovs 真正算出来并同步到渲染侧 compute_params。
    engine.recompute_blocking();

    // 诊断：set_level 后的 fovs 范围（对照 load 阶段的 fovs，确认参数是否导致 fov>1）。
    // fovs 全部 <1 才是正常防抖裁切；出现 >1 表示画面被迫缩小（黑边/全黑）。
    {
        let params = engine.params.read();
        let (fovs_min, fovs_max) = params.fovs.iter().copied().fold((f64::MAX, f64::MIN), |(mn, mx), v| (mn.min(v), mx.max(v)));
        let (mf_min, mf_max) = params.minimal_fovs.iter().copied().fold((f64::MAX, f64::MIN), |(mn, mx), v| (mn.min(v), mx.max(v)));
        println!("[gyroflow] 诊断 set_level后 fovs: {} 帧 [min={:.4} max={:.4}] | minimal_fovs [min={:.4} max={:.4}]",
                 params.fovs.len(), fovs_min, fovs_max, mf_min, mf_max);
    }
}

/// 设置 video_rotation（0/90/180/270 度）：gyroflow 内部 image_rotation 旋转输出内容 + IMU 对齐。
/// 必须随后调用 set_output_size 让输出宽高互换（90/270 → 竖），否则竖画面被挤压到横画布 1/3。
/// 上层 Swift 侧不得再对防抖帧做 .oriented 旋转（避免双重旋转）。
#[no_mangle]
pub unsafe extern "C" fn gf_engine_set_video_rotation(
    e: *mut GFEngine,
    rotation: f64,
) {
    if e.is_null() {
        return;
    }
    let engine = &(*e).inner;
    engine.set_video_rotation(rotation);
    // 关键修复：set_output_size 必须传【互换后】尺寸（90/270 → 竖 (h,w)）。
    // 之前传横尺寸 (w,h) 会被内部 letterbox 等比缩放（scale=min(ow/w, oh/h)=0.5625），
    // gyroflow 实际只渲染 2160x1215 → 塞进竖 stabBuffer 后内容占上 1/3 + 黑（1/3 问题根源）。
    let (w, h) = {
        let p = engine.params.read();
        (p.size.0.max(1), p.size.1.max(1))
    };
    let r = rotation.abs();
    let (ow, oh) = if r == 90.0 || r == 270.0 { (h, w) } else { (w, h) };
    engine.set_output_size(ow, oh);
    // 阻塞式全量重算（使 video_rotation + 输出尺寸进入渲染侧 compute_params）
    engine.recompute_blocking();
    println!("[gyroflow] 已设置 video_rotation={} + set_output_size({}x{})（90/270 互换宽高）", rotation, ow, oh);
}

/// 单帧防抖处理：in/out 均为 BGRA（stride ≥ width×4，同尺寸）。
/// timestamp_us 为原始时间轴微秒。
/// 返回 0=成功；非 0=失败。
#[no_mangle]
pub unsafe extern "C" fn gf_engine_process_frame(
    e: *mut GFEngine,
    timestamp_us: i64,
    input: *const u8,
    in_stride: c_int,
    output: *mut u8,
    out_stride: c_int,
    width: c_int,
    height: c_int,
) -> c_int {
    if e.is_null() || input.is_null() || output.is_null() {
        return -1;
    }
    let engine = &(*e).inner;

    let w = width.max(1) as usize;
    let h = height.max(1) as usize;
    let in_stride = in_stride.max((w * 4) as i32) as usize;

    // 输出尺寸取 gyroflow 的 output_size（set_video_rotation 后 90/270 会自动宽高互换为竖），
    // 与 Swift 侧竖方向的 stabBuffer 匹配；input 保持 sourceBuffer 原始方向（横）。
    let (ow, oh) = {
        let p = engine.params.read();
        (p.output_size.0.max(1), p.output_size.1.max(1))
    };
    let out_stride = out_stride.max((ow * 4) as i32) as usize;

    let in_len = in_stride * h;
    let out_len = out_stride * oh;

    // gyroflow 的 CPU 输入以 &mut [u8] 表达（实际只读），此处按 FFI 约定转换
    let in_slice: &mut [u8] = std::slice::from_raw_parts_mut(input as *mut u8, in_len);
    let out_slice: &mut [u8] = std::slice::from_raw_parts_mut(output, out_len);

    let mut buffers = Buffers {
        input: BufferDescription {
            size: (w, h, in_stride),
            rect: None,
            rotation: None,
            data: BufferSource::Cpu {
                buffer: in_slice,
            },
            texture_copy: false,
        },
        output: BufferDescription {
            size: (ow, oh, out_stride),
            rect: None,
            rotation: None,
            data: BufferSource::Cpu {
                buffer: out_slice,
            },
            texture_copy: false,
        },
    };

    // 静态帧计数：诊断防抖是否真在计算（每 60 帧打印 fov/min_fov/backend）
    static FRAME_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let fc = FRAME_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

    match engine.process_pixels::<BGRA8>(timestamp_us, None, &mut buffers) {
        Ok(info) => {
            // 注意：不再做 Y 镜像（video_rotation 方案下 image_rotation 已旋转内容，
            // 输出尺寸=set_output_size 互换后的竖/横尺寸，Y 镜像会破坏方向）。
            if fc % 60 == 1 {
                println!("[gyroflow] 帧 {} 防抖诊断: fov={:.4} min_fov={:.4} backend={} (fov<1 表示裁切缩放进行中)",
                         fc, info.fov, info.minimal_fov, info.backend);
            }
            0
        }
        Err(_) => -5,
    }
}
