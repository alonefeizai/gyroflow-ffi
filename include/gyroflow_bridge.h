// gyroflow_bridge.h — LutFTP 防抖桥接 C ABI 声明（Swift 通过 Bridging Header 导入）
#ifndef GYROFLOW_BRIDGE_H
#define GYROFLOW_BRIDGE_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* 不透明引擎句柄（内部为 gyroflow_core::StabilizationManager） */
typedef struct GFEngine GFEngine;

/* 创建/销毁引擎 */
GFEngine *gf_engine_create(void);
void      gf_engine_destroy(GFEngine *e);

/*
 * 载入视频并解析内嵌陀螺仪元数据。
 * url        : 沙盒内视频文件路径（C 字符串）
 * duration_ms: 视频时长（毫秒）
 * fps        : 视频帧率
 * frame_count: 视频总帧数
 * width,height: 视频**原始方向**像素尺寸（videoTrack.naturalSize，不含旋转；
 *               防抖在原始方向像素上处理，旋转由 Swift 侧 applyLUTsToVideo 完成）
 * has_motion : [out] 非 0 表示视频含可用陀螺仪数据（防抖前提）
 * 返回 0 = 成功；非 0 = 失败码（-1 空指针 / -2 非法路径 / -3 文件打开失败 / -4 陀螺仪解析失败）
 */
int gf_engine_load_video(GFEngine *e, const char *url,
                         double duration_ms, double fps, int frame_count,
                         int width, int height, int *has_motion);

/*
 * 设置防抖级别与变速倍率。
 * level      : 1=低 2=中 3=高（0 表示不启用，调用方不应调用本函数）
 * video_speed: Lut版视频变速倍率（0.3~3.0；用于时间轴映射，1.0 为不变速）
 */
void gf_engine_set_level(GFEngine *e, int level, double video_speed);

/*
 * 设置输出方向（video_rotation：0/90/180/270 度）。
 * gyroflow 渲染时原生旋转输出像素并自动互换输出宽高（90/270 时输出变竖）。
 * 调用后 App 侧无需再做方向处理；应在 load_video 之后、set_level 之前调用。
 */
void gf_engine_set_video_rotation(GFEngine *e, double rotation);

/*
 * 设置 IMU 旋转（pitch/roll/yaw，单位度）：校正机身 IMU 坐标差异。
 * 用途：A7M5 等素材若 IMU 元数据与 A7C2 差 180°（绕光轴），gyroflow 稳定输出会整体
 * 倒立但防抖补偿方向正常；对该类机型调用本函数补 180° 即可转正（绕哪个轴需真机微调）。
 * 三个角度全为 0 时清除设置（恢复默认；A7C2 等无需设置）。
 * 应在 load_video 之后、set_level 之前调用。
 */
void gf_engine_set_imu_rotation(GFEngine *e, double pitch_deg, double roll_deg, double yaw_deg);

/*
 * 单帧防抖处理。
 * timestamp_us: 原始时间轴微秒（变速时由调用方映射：compositionTime × speed）
 * input, in_stride  : 输入 BGRA 像素指针与行字节数（width×4 ≤ stride）
 * output, out_stride: 输出 BGRA 像素指针与行字节数（与输入同尺寸）
 * 返回 0 = 成功；非 0 = 失败
 */
int gf_engine_process_frame(GFEngine *e, int64_t timestamp_us,
                            const uint8_t *input, int in_stride,
                            uint8_t *output, int out_stride,
                            int width, int height);

#ifdef __cplusplus
}
#endif

#endif /* GYROFLOW_BRIDGE_H */
