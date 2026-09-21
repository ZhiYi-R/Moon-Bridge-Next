<script setup lang="ts">
// open=true 时初始展开；bare=true 去掉外壳与标题（供弹窗直接显示正文）
withDefaults(defineProps<{ open?: boolean; bare?: boolean }>(), { open: false, bare: false });
</script>

<template>
  <details class="text-left" :class="bare ? '' : 'rounded-md border'" :open="open">
    <!-- bare 时隐藏标题但保留 summary 元素，避免浏览器渲染默认的「详情」展开标签 -->
    <summary :class="['cursor-pointer px-3 py-2 text-sm font-medium', bare && 'hidden']">脚本编写指南</summary>
    <div class="space-y-3 text-xs leading-relaxed" :class="bare ? '' : 'border-t px-3 py-3'">
      <div class="space-y-1">
        <p class="font-medium">一、脚本里能拿到什么</p>
        <ul class="space-y-0.5 text-muted-foreground">
          <li><code class="font-mono">ctx.name</code>：卡片名</li>
          <li><code class="font-mono">ctx.key</code>：本次查询用的 API Key</li>
          <li><code class="font-mono">ctx.keys</code>：只装了当前这一个 key 的数组。一张卡填了几个 key，就会一个一个分开跑，脚本只管写单个 key 的情况。</li>
          <li><code class="font-mono">ctx.base_url</code>：卡片里填的查询地址，没填就是空字符串。脚本能访问哪些地址由程序的安全策略决定，不是想请求哪就请求哪。</li>
          <li><code class="font-mono">ctx.provider</code>：卡片引用的上游服务 key</li>
          <li><code class="font-mono">ctx.extra</code>：卡片「额外参数」框里的内容</li>
        </ul>
      </div>
      <div class="space-y-1">
        <p class="font-medium">二、调用约定</p>
        <p class="text-muted-foreground">
          脚本开头写 <code class="font-mono">MB = {}</code>，查询函数名固定是 <code class="font-mono">MB.query(ctx)</code>，
          程序调用它，拿返回的 Lua 表渲染界面。常用的字段：<code class="font-mono">status</code>（填 "ok" 或 "error"，不填当 ok）、
          <code class="font-mono">message</code>（失败原因）、<code class="font-mono">summary</code>（一句话备注，可不填）、
          <code class="font-mono">quotas</code>（余额条目列表，界面上画成环形图）。
        </p>
        <pre class="scrollbar-thin overflow-auto rounded border bg-muted/40 p-2 font-mono text-[11px]">{ type = "percentage", label = "Weekly", period_secs = 604800, used_percent = 43, reset_at = "2026/9/21 13:02" }
{ type = "quota", label = "余额", unit = "$", left_amount = 0 }</pre>
        <p class="text-muted-foreground">
          每条配额必须声明 <code class="font-mono">type</code>：<code class="font-mono">percentage</code>（百分比额度）、
          <code class="font-mono">quota</code>（金额额度）、<code class="font-mono">counter</code>（计数器，界面不展示）。
          字段名用 snake_case（used_percent、left_amount），显示时自动转 camelCase；百分比两个字段给一个就够。
          有滚动窗口的配额填 <code class="font-mono">period_secs</code>（窗口秒数），界面据此对照本地消耗。
          reset_at 可以直接写要显示的文字，也可以给 unix 时间戳的秒数，界面会换算成本地时间。
        </p>
        <p class="text-muted-foreground">
          请求头用键值对的数组：<code class="font-mono">&#123;&#123; "authorization", "Bearer " .. ctx.key &#125;&#125;</code>，每一项是「名字, 值」。
          上游返回的 JSON 程序已经解析好，直接从 <code class="font-mono">r.body</code> 取字段就行。
        </p>
      </div>
      <div class="space-y-1">
        <p class="font-medium">三、额外参数</p>
        <p class="text-muted-foreground">
          卡片的「额外参数」框里写了什么，脚本里就用 <code class="font-mono">ctx.extra.字段名</code> 读什么（比如换算比例、币种）；
          用不到就保留 <code class="font-mono">{}</code>。
          脚本可以引用一个 .lua 文件，也可以把代码直接贴在卡片里，二选一；套用模板会清空文件路径、改成贴代码。
        </p>
      </div>
    </div>
  </details>
</template>
