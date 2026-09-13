/**
 * 指针拖拽把手：返回 @pointerdown 处理器。
 * onStart 在拖拽开始时取初值；onMove 持续收到相对起点的位移；onEnd 在抬手时回调（适合写持久化）。
 */
export function usePointerDrag(
  onMove: (dx: number, dy: number) => void,
  options?: { onStart?: () => void; onEnd?: () => void },
) {
  return function startDrag(e: PointerEvent) {
    e.preventDefault();
    options?.onStart?.();
    const sx = e.clientX;
    const sy = e.clientY;
    const move = (ev: PointerEvent) => onMove(ev.clientX - sx, ev.clientY - sy);
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      options?.onEnd?.();
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
}
