/** Copy only after a user action. A denied/failed fallback is not a success. */
export async function copyPlainText(text: string): Promise<boolean> {
    try {
        if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
            return true;
        }
    } catch { /* Fall back for WebKit/clipboard permissions. */ }
    if (typeof document === 'undefined') return false;
    const previous = document.activeElement;
    const selection = document.getSelection();
    const ranges = selection ? Array.from({ length: selection.rangeCount }, (_, i) => selection.getRangeAt(i).cloneRange()) : [];
    const field = document.createElement('textarea');
    field.value = text;
    field.setAttribute('readonly', '');
    field.style.cssText = 'position:fixed;left:-9999px;opacity:0';
    try {
        document.body.appendChild(field);
        field.focus({ preventScroll: true });
        field.select();
        return document.execCommand('copy');
    } catch { return false; }
    finally {
        field.remove();
        // Focus/selection may have changed while copying; restoration is best effort.
        try {
            if (previous instanceof HTMLElement && previous.isConnected) previous.focus({ preventScroll: true });
            if (selection && ranges.length) {
                selection.removeAllRanges();
                for (const range of ranges) selection.addRange(range);
            }
        } catch { /* A detached selection must not change the copy result. */ }
    }
}
