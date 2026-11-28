/** Dragging across selectable text can dispatch a click on its enclosing row. */
export function stopSelectionClick(event: MouseEvent) {
    if (event.detail === 0 || !(event.target instanceof Element)) return;
    if (event.target.closest("button, a, input, textarea, select, [contenteditable]")) return;
    const selection = document.getSelection();
    if (!selection || selection.isCollapsed) return;
    for (let index = 0; index < selection.rangeCount; index++) {
        if (!selection.getRangeAt(index).intersectsNode(event.target)) continue;
        event.stopPropagation();
        return;
    }
}
