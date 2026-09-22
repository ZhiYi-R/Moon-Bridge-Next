import { onBeforeUnmount, watch, type Ref } from "vue";

/**
 * 滚轮翻页：滚动方向还有余量时让位原生滚动；滚到边沿
 * （自动分页的列表整页正好铺满，天然满足）才翻页。
 * 翻页后进入冷却期，避免触控板惯性一次滚过多页。
 */
export function useWheelPaging(
  target: Ref<HTMLElement | null>,
  opts: {
    canPrev: () => boolean;
    canNext: () => boolean;
    prev: () => void;
    next: () => void;
  },
) {
  let cooldownUntil = 0;

  function onWheel(e: WheelEvent) {
    const el = target.value;
    if (!el) return;
    const dy = e.deltaY * (e.deltaMode === 1 ? 16 : 1);
    const dir = dy > 0 ? 1 : dy < 0 ? -1 : 0;
    if (dir === 0 || Math.abs(dy) < 8) return;
    const atEdge =
      dir > 0
        ? el.scrollTop + el.clientHeight >= el.scrollHeight - 1
        : el.scrollTop <= 1;
    if (!atEdge) return;
    e.preventDefault();
    const now = performance.now();
    if (now < cooldownUntil) return;
    const turn = dir > 0 ? opts.next : opts.prev;
    if (!(dir > 0 ? opts.canNext() : opts.canPrev())) return;
    cooldownUntil = now + 350;
    turn();
  }

  // 容器可能随 v-if/v-else 重建，ref 变更时重新绑定
  watch(
    target,
    (el, prev) => {
      prev?.removeEventListener("wheel", onWheel);
      el?.addEventListener("wheel", onWheel, { passive: false });
    },
    { immediate: true, flush: "post" },
  );
  onBeforeUnmount(() => target.value?.removeEventListener("wheel", onWheel));
}
