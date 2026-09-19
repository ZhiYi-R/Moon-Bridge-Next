<template>
  <details class="rounded-md border text-left">
    <summary class="cursor-pointer px-3 py-2 text-sm font-medium">脚本编写指南</summary>
    <div class="space-y-3 border-t px-3 py-3 text-xs leading-relaxed">
      <div class="space-y-1">
        <p class="font-medium">一、输入参数 ctx</p>
        <ul class="space-y-0.5 text-muted-foreground">
          <li><code class="font-mono">ctx.name</code>：卡片名（key）</li>
          <li><code class="font-mono">ctx.key</code>：本次查询的 API Key</li>
          <li><code class="font-mono">ctx.keys</code>：当前 key 的单元素数组。多个 key 会逐个执行脚本，脚本只需按单个 key 编写。</li>
          <li><code class="font-mono">ctx.base_url</code>：卡片查询 URL；留空时为空字符串。请求地址须符合宿主的网络访问策略。</li>
          <li><code class="font-mono">ctx.provider</code>：上游服务 key</li>
          <li><code class="font-mono">ctx.extra</code>：额外参数 JSON 的解码值</li>
        </ul>
      </div>
      <div class="space-y-1">
        <p class="font-medium">二、返回值契约</p>
        <p class="text-muted-foreground">
          返回 table，常用字段：<code class="font-mono">status</code>（ok/error，缺省 ok）、
          <code class="font-mono">message</code>（失败原因）、<code class="font-mono">summary</code>（摘要）、
          <code class="font-mono">quotas</code>（配额条数组）。
        </p>
        <pre class="scrollbar-thin overflow-auto rounded border bg-muted/40 p-2 font-mono text-[11px]">{ label = "Weekly", used_percent = 43, reset_at = "2026/9/21 13:02" }
{ label = "余额", unit = "$", left_amount = 0 }</pre>
        <p class="text-muted-foreground">
          quota 使用 snake_case，宿主转换为前端 camelCase。百分比字段 used_percent / left_percent 给一个即可；
          金额字段 used_amount / left_amount 可各自独立返回，包括零值。reset_at 可给展示字符串或 unix 秒数字。
        </p>
        <p class="text-muted-foreground">
          脚本入口为 <code class="font-mono">MB = {}</code> 与 <code class="font-mono">function MB.query(ctx)</code>。
          HTTP 头使用 <code class="font-mono">&#123;&#123; "authorization", "Bearer " .. ctx.key &#125;&#125;</code> 数组形式；
          上游 JSON 响应的 <code class="font-mono">r.body</code> 已解码，可直接取字段。
        </p>
      </div>
      <div class="space-y-1">
        <p class="font-medium">三、额外参数 JSON</p>
        <p class="text-muted-foreground">
          用 <code class="font-mono">ctx.extra.字段名</code> 读取每张卡的配置（如换算比例、币种）；无需配置时保留 <code class="font-mono">{}</code>。
          文件引用和内联脚本二选一；使用模板会清空文件路径并切换到内联脚本。
        </p>
      </div>
    </div>
  </details>
</template>
