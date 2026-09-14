import type { Component } from "vue";
import SearchView from "./SearchView.vue";
import SearchCacheView from "./SearchCacheView.vue";
import ReadView from "./ReadView.vue";
import DiffView from "./DiffView.vue";
import WriteView from "./WriteView.vue";
import TerminalView from "./TerminalView.vue";

/**
 * 工具直供结构化视图注册表：DispatchResult.view.kind → 卡片组件。
 * 新增外观 = 后端在返回时塞 view:{kind:...} + 此处注册一行 + 新建对应组件；
 * kind 未注册或 view 为空时，前端回退通用文本启发式渲染，渐进不死。
 */
export const toolViewComponents: Record<string, Component> = {
    search: SearchView,
    "search-cache": SearchCacheView,
    read: ReadView,
    diff: DiffView,
    write: WriteView,
    terminal: TerminalView,
};
