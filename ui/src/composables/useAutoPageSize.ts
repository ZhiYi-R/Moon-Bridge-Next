import { onBeforeUnmount, ref, watch, type Ref } from "vue";

/**
 * 根据容器可视高度自动计算表格每页行数。
 *
 * 行高取首个数据行实测值（含边框）；可用高度 = 容器内容区高 − 表头 − 表格前置兄弟元素（如「添加报价」行）− 纵向内边距。
 * 通过 ResizeObserver 监听容器尺寸变化、MutationObserver 监听行增删后自动重测。
 * 要求容器高度由布局决定（flex 填充 / max-h），不随行数增长，否则分页与渲染会互相触发形成反馈循环。
 */
export function useAutoPageSize(container: Ref<HTMLElement | null>) {
  const pageSize = ref(20);
  let rowHeight = 0;
  let resizeObserver: ResizeObserver | null = null;
  let mutationObserver: MutationObserver | null = null;

  function measure() {
    const el = container.value;
    if (!el) return;
    const table = el.querySelector<HTMLElement>("table");
    const head = el.querySelector<HTMLElement>("thead");
    const row = el.querySelector<HTMLElement>("tbody tr");
    if (row) rowHeight = row.offsetHeight;
    if (!table || !head || rowHeight <= 0) return;
    const cs = getComputedStyle(el);
    let avail =
      el.clientHeight -
      parseFloat(cs.paddingTop) -
      parseFloat(cs.paddingBottom) -
      head.offsetHeight -
      2; // 少量余量，避免取整后溢出出现滚动条
    for (
      let sib = table.previousElementSibling as HTMLElement | null;
      sib;
      sib = sib.previousElementSibling as HTMLElement | null
    ) {
      avail -= sib.offsetHeight;
    }
    pageSize.value = Math.max(8, Math.floor(avail / rowHeight));
  }

  watch(
    container,
    (el) => {
      resizeObserver?.disconnect();
      mutationObserver?.disconnect();
      if (!el) return;
      resizeObserver = new ResizeObserver(measure);
      resizeObserver.observe(el);
      mutationObserver = new MutationObserver(measure);
      mutationObserver.observe(el, { childList: true, subtree: true });
      measure();
    },
    { immediate: true },
  );

  onBeforeUnmount(() => {
    resizeObserver?.disconnect();
    mutationObserver?.disconnect();
  });

  return { pageSize };
}
