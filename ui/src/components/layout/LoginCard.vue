<script setup lang="ts">
import { KeyRound } from "lucide-vue-next";
import { ref } from "vue";

import Button from "@/components/ui/Button.vue";
import Card from "@/components/ui/Card.vue";
import Input from "@/components/ui/Input.vue";
import Label from "@/components/ui/Label.vue";
import { errMsg, gatewayApi } from "@/lib/api";
import { clearToken, getToken, setToken } from "@/lib/web";

const emit = defineEmits<{ authenticated: [] }>();

const token = ref("");
const error = ref<string | null>(null);
const busy = ref(false);

/** 保存令牌并试探一次接口：令牌错误立即反馈，避免进入空壳页面。 */
async function submit() {
  const value = token.value.trim();
  if (!value) {
    error.value = "请输入管理令牌";
    return;
  }
  busy.value = true;
  const prev = getToken();
  setToken(value);
  try {
    await gatewayApi.status();
    error.value = null;
    emit("authenticated");
  } catch (e) {
    // 校验失败（含 401）时回滚，保持遮罩显示
    if (prev) setToken(prev);
    else clearToken();
    error.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-background/95 backdrop-blur-sm">
    <Card class="w-full max-w-sm p-6">
      <div class="flex items-center gap-2">
        <KeyRound class="size-4 text-muted-foreground" />
        <h2 class="card-title">需要管理令牌</h2>
      </div>
      <p class="mt-2 text-sm text-muted-foreground">
        该页面为 web 管理模式，请输入服务端启动时配置的 admin token。
      </p>
      <form class="mt-4 space-y-1.5" @submit.prevent="submit">
        <Label for="mb-token">管理令牌</Label>
        <Input
          id="mb-token"
          v-model="token"
          type="password"
          placeholder="MOONBRIDGE_ADMIN_TOKEN"
          autocomplete="current-password"
          autofocus
        />
        <p v-if="error" class="pt-1 text-xs text-destructive">{{ error }}</p>
        <Button type="submit" class="mt-3 w-full" :disabled="busy">
          {{ busy ? "验证中…" : "进入管理台" }}
        </Button>
      </form>
    </Card>
  </div>
</template>
