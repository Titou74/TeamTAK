// The anti-bot challenge of the sign-in page (crates/core/src/challenge.rs): fetched and
// solved in the background as soon as the page opens, so that it is ready when the user
// asks for a code. Each request for a code uses up one; the next is prepared right away,
// for a "send a new code".
import { useEffect, useState } from "react";

import { api, ApiError } from "./api";
import { solveChallenge } from "./proof-of-work";
import type { SolveRequest } from "./proof-of-work.worker";
import type { LoginProof } from "./types";

/** A solved challenge with less validity left than this is replaced before being sent. */
const MARGIN_MS = 30_000;

interface Prepared {
  proof: LoginProof;
  /** When the server stops accepting it, by this browser's clock. */
  deadline: number;
}

/** Runs the search in a worker; on the page itself when workers are unavailable. */
function solve(request: SolveRequest, signal: AbortSignal): Promise<number | null> {
  const here = () => solveChallenge(request.salt, request.challenge, request.maxNumber);
  return new Promise((resolve, reject) => {
    if (signal.aborted) return reject(signal.reason);
    let worker: Worker;
    try {
      worker = new Worker(new URL("./proof-of-work.worker.ts", import.meta.url), { type: "module" });
    } catch {
      return resolve(here());
    }
    const stop = () => {
      worker.terminate();
      signal.removeEventListener("abort", abort);
    };
    const abort = () => {
      stop();
      reject(signal.reason);
    };
    signal.addEventListener("abort", abort);
    worker.onmessage = (event: MessageEvent<number | null>) => {
      stop();
      resolve(event.data);
    };
    worker.onerror = () => {
      stop();
      resolve(here());
    };
    worker.postMessage(request);
  });
}

async function prepare(signal: AbortSignal): Promise<Prepared> {
  const challenge = await api.loginChallenge();
  const deadline = Date.now() + challenge.expires_in * 1000;
  const number = await solve(
    { salt: challenge.salt, challenge: challenge.challenge, maxNumber: challenge.max_number },
    signal,
  );
  if (number === null) throw new ApiError("challenge_failed", 0);
  return { proof: { salt: challenge.salt, challenge: challenge.challenge, number, signature: challenge.signature }, deadline };
}

export class LoginProofs {
  private next: Promise<Prepared> | null = null;
  private controller = new AbortController();

  /** Starts preparing the next proof, unless one is already on its way. */
  prepare(): void {
    if (this.next) return;
    const next = prepare(this.controller.signal);
    // A failure surfaces when the proof is taken, which then starts over.
    next.catch(() => {
      if (this.next === next) this.next = null;
    });
    this.next = next;
  }

  /** A proof to send with a request for a code. Waits for it if it is still being solved. */
  async take(): Promise<LoginProof> {
    const signal = this.controller.signal;
    const pending = this.next ?? prepare(signal);
    this.next = null;
    let prepared: Prepared;
    try {
      prepared = await pending;
    } catch (e) {
      if (signal.aborted) throw e;
      // Prepared while offline, for example: one more go.
      prepared = await prepare(signal);
    }
    // The page was left open: this one is about to expire.
    if (prepared.deadline - Date.now() < MARGIN_MS) prepared = await prepare(signal);
    this.prepare();
    return prepared.proof;
  }

  /** Stops the work under way; `prepare` starts afresh. */
  dispose(): void {
    this.controller.abort();
    this.controller = new AbortController();
    this.next = null;
  }
}

/** The proofs of the sign-in page, prepared from the moment it is shown. */
export function useLoginProofs(): LoginProofs {
  const [proofs] = useState(() => new LoginProofs());
  useEffect(() => {
    proofs.prepare();
    return () => proofs.dispose();
  }, [proofs]);
  return proofs;
}
