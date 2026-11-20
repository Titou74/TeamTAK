// Solves an anti-bot challenge away from the page, so typing stays smooth while it works.
import { solveChallenge } from "./proof-of-work";

export interface SolveRequest {
  salt: string;
  challenge: string;
  maxNumber: number;
}

const scope = self as unknown as {
  onmessage: ((event: MessageEvent<SolveRequest>) => void) | null;
  postMessage: (number: number | null) => void;
};

scope.onmessage = (event) => {
  const { salt, challenge, maxNumber } = event.data;
  scope.postMessage(solveChallenge(salt, challenge, maxNumber));
};
