import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** 合并 Tailwind class（shadcn-vue 约定）。 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** 把 unix 秒时间戳格式化为本地可读时间。 */
export function formatTime(unixSec: number): string {
  if (!unixSec) return "—";
  return new Date(unixSec * 1000).toLocaleString();
}

/** 把 unix 毫秒时间戳格式化为本地可读时间。 */
export function formatTimeMs(unixMs: number): string {
  if (!unixMs) return "—";
  return new Date(unixMs).toLocaleString();
}

/** 格式化字节数为可读体积。 */
export function formatBytes(n: number): string {
  if (!n) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  const v = n / Math.pow(1024, i);
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

/** 格式化 token 数：自适应单位（<1k 原值，≥1k K，≥1M M），两位小数。 */
export function formatTokens(n: number): string {
  const v = n ?? 0;
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(2)}M`;
  if (v >= 1_000) return `${(v / 1_000).toFixed(2)}K`;
  return String(v);
}

/** 格式化延迟：≥1s 用秒（两位小数），否则整毫秒。 */
export function formatLatency(ms: number): string {
  if (!ms) return "—";
  return ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(2)}s`;
}

/** 上下文窗口缩写：自适应单位（200000 → 200K，1.5M → 1.5M）。 */
export function formatCtx(n: number | null | undefined): string {
  if (!n) return "—";
  if (n >= 1_000_000) {
    const v = n / 1_000_000;
    return `${Number.isInteger(v) ? v : v.toFixed(1)}M`;
  }
  if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
  return String(n);
}
