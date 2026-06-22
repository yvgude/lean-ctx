/** Error hierarchy for the lean-ctx SDK. */

export class LeanCtxError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "LeanCtxError";
  }
}

/** The local lean-ctx proxy could not be reached (daemon down / wrong URL). */
export class LeanCtxConnectionError extends LeanCtxError {
  constructor(message: string) {
    super(message);
    this.name = "LeanCtxConnectionError";
  }
}

/** The proxy rejected the request (missing or invalid session token). */
export class LeanCtxAuthError extends LeanCtxError {
  constructor(message: string) {
    super(message);
    this.name = "LeanCtxAuthError";
  }
}
