import { ref } from "vue";

/**
 * 主题状态：默认深色（index.html 内联脚本在首帧前已按 localStorage 设好 html.dark，
 * 这里只负责读取结果并响应切换）。CodeMirror 等第三方组件监听 isDark 做联动。
 */
const STORAGE_KEY = "mb-theme";

export const isDark = ref(true);

function apply() {
  document.documentElement.classList.toggle("dark", isDark.value);
}

/** 应用挂载前调用：读 localStorage（无记录保持深色），同步 html class 与 ref。 */
export function initTheme() {
  isDark.value = localStorage.getItem(STORAGE_KEY) !== "light";
  apply();
}

export function toggleTheme() {
  isDark.value = !isDark.value;
  localStorage.setItem(STORAGE_KEY, isDark.value ? "dark" : "light");
  apply();
}
