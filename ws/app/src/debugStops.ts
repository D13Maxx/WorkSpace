// Which debugger stops to show. lldb places one line's breakpoint at every
// piece of code the line became (println! makes several) and stops at each:
// "breakpoint 1.1", "1.2", "1.3". After Continue, another piece of the same
// line is not a new stop; a loop coming back to a piece already passed is.

export interface LastStop {
  where: string;
  breakpoint: string;
  /** The pieces of that breakpoint passed since it was shown. */
  seen: Set<string>;
}

/** Whether to go straight on from this stop, and what to remember. */
export function repeatStop(
  last: LastStop | null,
  stop: { resumedBy: "continue" | "step" | null; reason?: string; where: string; description?: string },
): { skip: boolean; last: LastStop | null } {
  const spot = /breakpoint (\d+)\.(\d+)/.exec(stop.description ?? "");
  if (
    stop.reason === "breakpoint" &&
    stop.resumedBy === "continue" &&
    spot &&
    last &&
    stop.where === last.where &&
    spot[1] === last.breakpoint &&
    !last.seen.has(spot[2])
  ) {
    last.seen.add(spot[2]);
    return { skip: true, last };
  }
  return { skip: false, last: spot ? { where: stop.where, breakpoint: spot[1], seen: new Set([spot[2]]) } : null };
}
