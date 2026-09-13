import { reactive } from "vue";

export type ToastType = "success" | "error" | "info";

export interface ToastItem {
  id: number;
  type: ToastType;
  message: string;
}

// 模块级单例：配合 ToastHost（App.vue 挂载一次）提供全局 toast。
const toasts = reactive<ToastItem[]>([]);
let seq = 0;

export function useToast() {
  function dismiss(id: number) {
    const i = toasts.findIndex((t) => t.id === id);
    if (i >= 0) toasts.splice(i, 1);
  }

  /** 弹一条 toast，timeout ms 后自动消失（0 = 常驻，需手动关闭）。 */
  function push(type: ToastType, message: string, timeout = 3200): number {
    const id = ++seq;
    toasts.push({ id, type, message });
    if (timeout > 0) setTimeout(() => dismiss(id), timeout);
    return id;
  }

  return {
    toasts,
    dismiss,
    success: (m: string, t?: number) => push("success", m, t),
    error: (m: string, t?: number) => push("error", m, t ?? 5000),
    info: (m: string, t?: number) => push("info", m, t),
  };
}
