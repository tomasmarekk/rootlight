// Keeps session-known operation metadata in memory and out of browser persistence.

import { useCallback, useMemo, useState, type ReactNode } from "react";

import type { ProjectIndexAdmission, RepositoryOperation } from "../api/contracts";
import {
  OperationContext,
  type OperationContextValue,
  type SessionOperation,
} from "./operation-context";

const maximumSessionOperations = 64;

export function OperationProvider({ children }: { children: ReactNode }) {
  const [operations, setOperations] = useState<SessionOperation[]>([]);

  const register = useCallback((admission: ProjectIndexAdmission, requestId: string) => {
    setOperations((current) => {
      const existing = current.find(
        (operation) => operation.admission.operationId === admission.operationId,
      );
      if (existing !== undefined) {
        return current.map((operation) =>
          operation === existing ? { ...operation, admission, requestId } : operation,
        );
      }
      return [{ admission, requestId }, ...current].slice(0, maximumSessionOperations);
    });
  }, []);

  const update = useCallback((operationId: string, status: RepositoryOperation) => {
    setOperations((current) =>
      current.map((operation) => {
        if (operation.admission.operationId === operationId) {
          return shouldReplaceOperation(operation.status, status)
            ? { ...operation, status }
            : operation;
        }
        const semanticOperationId =
          operation.status?.semanticOperationId ?? operation.admission.semanticOperationId;
        if (semanticOperationId !== operationId) {
          return operation;
        }
        return shouldReplaceOperation(operation.semanticStatus, status)
          ? { ...operation, semanticStatus: status }
          : operation;
      }),
    );
  }, []);

  const dismiss = useCallback((operationId: string) => {
    setOperations((current) =>
      current.filter((operation) => operation.admission.operationId !== operationId),
    );
  }, []);

  const value = useMemo<OperationContextValue>(
    () => ({ operations, register, update, dismiss }),
    [dismiss, operations, register, update],
  );
  return <OperationContext.Provider value={value}>{children}</OperationContext.Provider>;
}

function shouldReplaceOperation(
  current: RepositoryOperation | undefined,
  next: RepositoryOperation,
): boolean {
  if (current === undefined) {
    return true;
  }
  const revisionOrder = BigInt(next.revision) - BigInt(current.revision);
  if (revisionOrder < 0n) {
    return false;
  }
  return (
    revisionOrder > 0n ||
    current.state !== next.state ||
    current.cancellationRequested !== next.cancellationRequested
  );
}
