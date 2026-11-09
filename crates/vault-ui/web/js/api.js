/* Talks to the local Rust core. Every failure becomes an ApiError whose message is safe to show. */

const params = new URLSearchParams(location.search);
export const isStatic = params.has("static"); // ?static: no live stream or polling (used for screenshots)

let token = params.get("t") || "";
try {
  if (token) sessionStorage.setItem("avault_t", token);
  else token = sessionStorage.getItem("avault_t") || "";
} catch (_) { /* the token then only lives for this page load */ }
if (params.get("t")) history.replaceState(null, "", location.pathname + location.hash);

export const hasToken = () => token !== "";
export const eventsUrl = () => "/api/events?t=" + encodeURIComponent(token);

export class ApiError extends Error {
  constructor(message, status = 0) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

const STATUS_TEXT = {
  401: "This session has expired. Reopen the app from the link avault prints.",
  403: "That request was refused.",
  404: "That wasn't found.",
  409: "That can't be done right now.",
};

function messageFor(status, serverMessage) {
  if (serverMessage && status < 500) return serverMessage;
  if (status >= 500) return serverMessage || "Something went wrong on this device. Try again.";
  return STATUS_TEXT[status] || `The request failed (HTTP ${status}).`;
}

/** Fetch JSON (or text for non-JSON replies). Throws ApiError. */
export async function api(path, { method = "GET", body } = {}) {
  let res;
  try {
    res = await fetch(path, {
      method,
      headers: { "X-Token": token, "Content-Type": "application/json" },
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch (_) {
    throw new ApiError("Can't reach the vault service. Check that avault is still running.", 0);
  }
  if (!res.ok) {
    let serverMessage = "";
    try { serverMessage = (await res.json()).error || ""; } catch (_) { /* not JSON: use the status text */ }
    throw new ApiError(messageFor(res.status, serverMessage), res.status);
  }
  return (res.headers.get("content-type") || "").includes("json") ? res.json() : res.text();
}

/** A message for any thrown value, never a stack trace. */
export function friendlyError(e) {
  if (e instanceof ApiError) return e.message;
  return "Something went wrong. Try again.";
}
