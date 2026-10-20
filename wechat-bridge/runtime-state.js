import fs from "node:fs";

const mapNames = ["activeRuns", "pendingQuestions", "taskQueues", "lastResults", "queuePauses"];

export function restoreRuntime(saved = {}) {
  return Object.fromEntries(mapNames.map((name) => [name, new Map(
    (Array.isArray(saved?.[name]) ? saved[name] : [])
      .filter((entry) => Array.isArray(entry) && entry.length === 2 && typeof entry[0] === "string")
  )]));
}

export function snapshotRuntime(runtime) {
  return Object.fromEntries(mapNames.map((name) => [name, [...runtime[name]]]));
}

// 同目录临时文件 + 原子替换，进程中断不能留下写到一半的绑定文件。
export function saveBindings(file, state, runtime) {
  const temporary = `${file}.tmp`;
  const fd = fs.openSync(temporary, "w", 0o600);
  try {
    fs.writeFileSync(fd, JSON.stringify({ ...state, runtime: snapshotRuntime(runtime) }, null, 2));
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
  fs.renameSync(temporary, file);
}

// 仅在实时账本明确不存在时回查同一 run 的持久记录，不用“最近一条”猜任务归属。
export async function readRun(api, entry) {
  try {
    return await api("GET", `/channel/run/${entry.runId}`);
  } catch (error) {
    if (error.status !== 404 && !String(error.message).includes("任务不存在")) throw error;
    let trace;
    try {
      ({ trace } = await api("GET", `/channel/trace/${entry.runId}`));
    } catch (traceError) {
      if (traceError.status !== 404) throw traceError;
    }
    if (!trace || !entry.sessionId || trace.id !== entry.runId || trace.session_id !== entry.sessionId) {
      return { status: "unknown", error: "实时任务记录不存在，且没有可精确关联的执行记录。" };
    }
    const completion = trace.completion || {};
    const summary = typeof completion.summary === "string" ? completion.summary : "";
    const details = typeof completion.details === "string" ? completion.details : "";
    const body = summary && details && summary !== details ? `${summary}\n\n${details}` : details || summary;
    if (["succeeded", "failed", "cancelled"].includes(trace.status)) {
      return {
        status: trace.status === "succeeded" ? "done" : trace.status,
        session_id: trace.session_id, result: body,
        error: summary || "执行记录未保存错误正文，请在桌面端查看。",
      };
    }
    return { status: "unknown", error: "实时任务记录不存在，持久执行记录尚无终态，无法确认是否完成。" };
  }
}
