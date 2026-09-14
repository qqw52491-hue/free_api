import { ref, type Ref } from "vue";

/**
 * 卡片内联展开/收起的组件本地状态。
 * 约定：时间线默认收起（仅头部）；diff 类默认展开；失败类自动展开。
 * 详情态（detail=true）由父级传入时强制视为展开——抽屉里总是全量。
 */
export function useExpandable(defaultExpanded: boolean): {
    expanded: Ref<boolean>;
    toggle: () => void;
} {
    const expanded = ref(defaultExpanded);
    const toggle = () => {
        expanded.value = !expanded.value;
    };
    return { expanded, toggle };
}
