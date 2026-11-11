// A command's failure, in one shape for every caller.
//
// Session and project commands reject with `{ kind, message, ... }`; a few
// older ones reject with a plain string. Wrapping the object keeps
// `String(cause)` and `${cause}` printing the message, while `cause.kind`
// stays available to the callers that branch on it.

export class CommandFailure extends Error {
  constructor(cause) {
    super(cause.message);
    Object.assign(this, cause);
    this.name = "CommandFailure";
  }

  toString() {
    return this.message;
  }
}

// Returns a `CommandFailure` for a typed rejection, and anything else
// (a string, an `Error`) unchanged.
export function commandFailure(cause) {
  const typed = cause && typeof cause === "object" && !(cause instanceof Error)
    && typeof cause.kind === "string" && typeof cause.message === "string";
  return typed ? new CommandFailure(cause) : cause;
}

// What the page does about a failed session command: nothing while the
// project is closing, forget a session no open project knows, otherwise
// show the message.
export function failureAction(cause) {
  if (cause?.kind === "shutting_down") return "ignore";
  if (cause?.kind === "unknown_session") return "forget";
  return "show";
}
