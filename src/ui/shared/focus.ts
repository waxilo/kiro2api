// 焦点快照：整块换 innerHTML 前记下焦点与选区，换完再放回去。
//
// 外壳重绘（以及弹窗宿主重建）都会把被替换掉的焦点元素一起扔掉 —— 端口输入框正在
// 编辑时被后台事件触发一次重绘，光标就会跳走。这里的两个函数成对使用：
// `captureFocus(host)` 在替换前调用，`restoreFocus(host, snapshot)` 在替换后调用。
//
// 焦点落在**没有 id** 的按钮上时，`data-refocus` 指名渲染后该把焦点还给谁。
// 「清空搜索」就是这种：点它的人下一步一定是接着改那个输入框，
// 不还焦点的话光标会掉到 body 上，得再点一次输入框才能继续打字。

export interface FocusSnapshot {
  id: string;
  start: number | null;
  end: number | null;
}

export function captureFocus(host: HTMLElement): FocusSnapshot | null {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || !host.contains(active)) return null;
  const id = active.id || active.dataset.refocus || '';
  if (!id) return null;
  const field = active as HTMLInputElement;
  const hasSelection = typeof field.selectionStart === 'number';
  return {
    id,
    start: hasSelection ? field.selectionStart : null,
    end: hasSelection ? field.selectionEnd : null,
  };
}

export function restoreFocus(host: HTMLElement, snapshot: FocusSnapshot | null): void {
  if (!snapshot) return;
  // `CSS.escape` 是因为 id 里可能有 `:` 这类需要转义的字符（本项目里没有，但别赌）
  const next = host.querySelector<HTMLElement>(`#${CSS.escape(snapshot.id)}`);
  if (!next) return;
  next.focus({ preventScroll: true });
  if (snapshot.start !== null && next instanceof HTMLInputElement) {
    // 夹到新长度：搜索框会把非 ASCII 字符裁掉，重绘后的值可能比快照里的光标位置还短
    const end = snapshot.end ?? snapshot.start;
    const length = next.value.length;
    next.setSelectionRange(Math.min(snapshot.start, length), Math.min(end, length));
  }
}
