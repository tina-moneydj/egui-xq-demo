// Swift ↔ Rust（mobile/src/ioshost.rs 的 C 介面）
#import <UIKit/UIKit.h>
#include <stdint.h>
typedef struct XQEguiHost XQEguiHost;
XQEguiHost *xq_egui_create(void *metal_layer, uint32_t w_px, uint32_t h_px, float scale);
void xq_egui_resize(XQEguiHost *h, uint32_t w_px, uint32_t h_px, float scale);
void xq_egui_touch(XQEguiHost *h, uint64_t id, uint32_t phase, float x, float y, float force);
void xq_egui_text(XQEguiHost *h, const char *utf8);
void xq_egui_backspace(XQEguiHost *h);
/// 回傳位元：1＝這次有畫，2＝要叫出鍵盤
uint32_t xq_egui_frame(XQEguiHost *h);
void xq_egui_wake(XQEguiHost *h);
