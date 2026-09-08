import { reactive } from "vue";

export interface ConfirmOptions {
  title?: string;
  message: string;
  confirmText?: string;
  /** 危险操作：确认按钮显示为红色 destructive。默认 true。 */
  danger?: boolean;
}

interface ConfirmState {
  open: boolean;
  title: string;
  message: string;
  confirmText: string;
  danger: boolean;
  resolve: ((v: boolean) => void) | null;
}

// 模块级单例状态：配合 ConfirmHost（App.vue 挂载一次）提供全局 confirm()。
const state = reactive<ConfirmState>({
  open: false,
  title: "确认操作",
  message: "",
  confirmText: "确认",
  danger: true,
  resolve: null,
});

export function useConfirm() {
  /** 弹出确认框，resolve 用户选择（true=确认 / false=取消）。 */
  function confirm(opts: ConfirmOptions): Promise<boolean> {
    state.title = opts.title ?? "确认操作";
    state.message = opts.message;
    state.confirmText = opts.confirmText ?? "确认";
    state.danger = opts.danger ?? true;
    state.open = true;
    return new Promise((resolve) => {
      state.resolve = resolve;
    });
  }

  /** ConfirmHost 内部使用：结束等待并关闭。 */
  function settle(value: boolean) {
    state.resolve?.(value);
    state.resolve = null;
    state.open = false;
  }

  return { state, confirm, settle };
}
