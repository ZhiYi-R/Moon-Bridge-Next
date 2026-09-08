import { createRouter, createWebHashHistory, type RouteRecordRaw } from "vue-router";

// 侧边栏导航项（顺序即菜单顺序）。图标名对应 lucide-vue-next。
export interface NavMeta {
  title: string;
  icon: string;
  /** 满版布局：去掉 main 的内边距，内容直接打满（适用于全屏主从面板）。 */
  flush?: boolean;
}

const routes: RouteRecordRaw[] = [
  { path: "/", redirect: "/dashboard" },
  {
    path: "/dashboard",
    name: "dashboard",
    component: () => import("@/views/DashboardView.vue"),
    meta: { title: "仪表盘", icon: "Gauge" } satisfies NavMeta,
  },
  {
    path: "/providers",
    name: "providers",
    component: () => import("@/views/ProvidersView.vue"),
    meta: { title: "上游服务", icon: "Server", flush: true } satisfies NavMeta,
  },
  {
    path: "/models",
    name: "models",
    component: () => import("@/views/ModelsView.vue"),
    meta: { title: "模型", icon: "Boxes", flush: true } satisfies NavMeta,
  },
  {
    path: "/routes",
    name: "routes",
    component: () => import("@/views/RoutesView.vue"),
    meta: { title: "路由", icon: "GitBranch", flush: true } satisfies NavMeta,
  },
  {
    path: "/plugins",
    name: "plugins",
    component: () => import("@/views/PluginsView.vue"),
    meta: { title: "插件", icon: "Puzzle", flush: true } satisfies NavMeta,
  },
  {
    path: "/usage",
    name: "usage",
    component: () => import("@/views/UsageView.vue"),
    meta: { title: "用量", icon: "BarChart3" } satisfies NavMeta,
  },
  {
    path: "/traces",
    name: "traces",
    component: () => import("@/views/TracesView.vue"),
    meta: { title: "调用追踪", icon: "ScrollText", flush: true } satisfies NavMeta,
  },
  {
    path: "/settings",
    name: "settings",
    component: () => import("@/views/SettingsView.vue"),
    meta: { title: "设置", icon: "Settings" } satisfies NavMeta,
  },
  { path: "/:pathMatch(.*)*", redirect: "/dashboard" },
];

const router = createRouter({
  // Tauri 以自定义协议加载本地资源，hash 模式避免刷新/深链 404
  history: createWebHashHistory(),
  routes,
});

export default router;
