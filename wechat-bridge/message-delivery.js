// 单条保持原有 3500 UTF-16 单位预算；长消息分段而非丢弃尾部。
export const MAX_MESSAGE_LENGTH = 3500;

export function splitWechatText(value, maxLength = MAX_MESSAGE_LENGTH) {
  if (!Number.isInteger(maxLength) || maxLength < 64) {
    throw new RangeError("maxLength must be an integer >= 64");
  }
  const text = String(value ?? "");
  if (!text) return [];
  if (text.length <= maxLength) return [text];

  // 为分段编号留出空间；保留原文空白，不拆开 emoji 的代理对。
  const budget = maxLength - 32;
  const chunks = [];
  let offset = 0;
  while (offset < text.length) {
    let end = Math.min(offset + budget, text.length);
    if (end < text.length) {
      const prefix = text.slice(offset, end);
      const paragraph = prefix.lastIndexOf("\n\n");
      const newline = prefix.lastIndexOf("\n");
      const boundary = paragraph >= budget / 2 ? paragraph + 2
        : newline >= budget / 2 ? newline + 1 : prefix.length;
      end = offset + boundary;
      const last = text.charCodeAt(end - 1);
      const next = text.charCodeAt(end);
      if (last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 && next <= 0xdfff) end--;
    }
    chunks.push(text.slice(offset, end));
    offset = end;
  }
  return chunks.map((chunk, index) => `（${index + 1}/${chunks.length}）\n${chunk}`);
}

// 共享发送队列防止不同通知的分段相互穿插。失败停止本条消息，队列仍可继续。
// 文本发送不自动重试，避免已送达但丢失回执时重复发送。
export function createMessageDelivery(send, { onMediaError = () => {} } = {}) {
  let tail = Promise.resolve();
  return (text, media) => {
    const operation = tail.then(async () => {
      const parts = splitWechatText(text);
      if (!parts.length && media) parts.push("");
      for (const [index, part] of parts.entries()) {
        if (index === 0 && media) {
          try {
            await send({ text: part, media });
          } catch (error) {
            onMediaError(error);
            if (part) await send(part);
            else throw error;
          }
        } else {
          await send(part);
        }
      }
    });
    tail = operation.catch(() => {});
    return operation;
  };
}
