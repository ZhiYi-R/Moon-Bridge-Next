import { onBeforeUnmount, ref, watch, type Ref } from "vue";

/**
 * 根据容器可视高度自动计算表格每页行数。
 *
 * 行高取首个数据行实测值（含边框）；可用高度 = 容器内容区高 − 表头 − 表格前置兄弟元素（如「添加报价」行）− 纵向内边距。
 * 通过 ResizeObserver 监听容器尺寸变化、MutationObserver 监听行增删后自动重测，rAF 合帧防抖。
 * 要求容器高度由布局决定（flex 填充 / max-h），不随行数增长，否则分页与渲染会互相触发形成反馈循环。
 * 传入 page 时，行数变化会把页号换算到「当前页首行」仍可见的位置，避免正在浏览的内容漂走。
 * rowSelector 默认 `tbody tr`；非表格列表（如 ul > li）传入对应选择器。
 */
export function useAutoPageSize(
  container: Ref<HTMLElement | null>,
  page?: Ref<number>,
  rowSelector = "tbody tr",
) {
  const pageSize = ref(20);
  /** 容器内表格区域的实测可用高度（px），供按块估算高度的分页场景使用。 */
  const availHeight = ref(0);
  let rowHeight = 0;
  let resizeObserver: ResizeObserver | null = null;
  let mutationObserver: MutationObserver | null = null;
  let raf = 0;

  function measure() {
    const el = container.value;
    if (!el) return;
    const head = el.querySelector<HTMLElement>("thead");
    const row = el.querySelector<HTMLElement>(rowSelector);
    // 非表格列表时取行的父容器（ul 等），供前置兄弟元素扣高
    const table = el.querySelector<HTMLElement>("table") ?? row?.parentElement ?? null;
    if (row) {
      rowHeight = row.offsetHeight;
      // 间距（space-y/gap/margin）在行盒外：有两行时用行距作真实行高，否则会逐行累计溢出
      const sib = row.nextElementSibling as HTMLElement | null;
      if (sib) rowHeight = Math.max(rowHeight, sib.offsetTop - row.offsetTop);
    }
    const cs = getComputedStyle(el);
    let avail =
      el.clientHeight -
      parseFloat(cs.paddingTop) -
      parseFloat(cs.paddingBottom) -
      (head?.offsetHeight ?? 0) -
      2; // 少量余量，避免取整后溢出出现滚动条
    for (
      let sib = table?.previousElementSibling as HTMLElement | null;
      sib;
      sib = sib.previousElementSibling as HTMLElement | null
    ) {
      avail -= sib.offsetHeight;
    }
    availHeight.value = Math.max(0, avail);
    if (!table || rowHeight <= 0) return;
    // 下限为 1：只要高于实际可容纳行数，渲染就会溢出——滚动条和翻页同时出现。
    // 连 1 行都放不下的极端矮容器才回退为滚动；上限防过大的表一次渲染过多。
    const next = Math.max(1, Math.min(200, Math.floor(avail / rowHeight)));
    if (next === pageSize.value) return;
    if (page) {
      const first = (page.value - 1) * pageSize.value;
      page.value = Math.floor(first / next) + 1;
    }
    pageSize.value = next;
  }

  /** rAF 合帧：一次 resize/行增删触发的多次测量合并为一帧一次。 */
  function schedule() {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      measure();
    });
  }

  watch(
    container,
    (el) => {
      resizeObserver?.disconnect();
      mutationObserver?.disconnect();
      if (!el) return;
      resizeObserver = new ResizeObserver(schedule);
      resizeObserver.observe(el);
      mutationObserver = new MutationObserver(schedule);
      mutationObserver.observe(el, { childList: true, subtree: true });
      measure();
    },
    { immediate: true },
  );

  onBeforeUnmount(() => {
    resizeObserver?.disconnect();
    mutationObserver?.disconnect();
    if (raf) cancelAnimationFrame(raf);
  });

  return { pageSize, availHeight };
}
