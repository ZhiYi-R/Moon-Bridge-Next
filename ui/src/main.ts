import { createPinia } from "pinia";
import { createApp } from "vue";

import { listen } from "@tauri-apps/api/event";

import App from "./App.vue";
import { isTauriRuntime } from "./lib/api";
import router from "./router";
import "./style.css";

const app = createApp(App);

app.use(createPinia());
app.use(router);

if (isTauriRuntime) {
  // 禁用窗口级右键菜单（WebView 默认菜单）；输入框/编辑器内保留原生菜单以便复制粘贴
  window.addEventListener("contextmenu", (e) => {
    const t = e.target as HTMLElement | null;
    if (t?.closest("input, textarea, [contenteditable]")) return;
    e.preventDefault();
  });

  // 托盘「打开标签页」菜单的跳转事件（Rust 侧先显示窗口再 emit）
  listen<string>("navigate-tab", (event) => {
    router.push(event.payload).catch(() => {});
  }).catch(() => {});
}

app.mount("#app");
