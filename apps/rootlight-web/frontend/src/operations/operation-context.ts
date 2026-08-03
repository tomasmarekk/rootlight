// Defines the in-memory operation store shared by authenticated product routes.

import { createContext, useContext } from "react";

import type { ProjectIndexAdmission, RepositoryOperation } from "../api/contracts";

export type SessionOperation = {
  admission: ProjectIndexAdmission;
  requestId: string;
  status?: RepositoryOperation;
  semanticStatus?: RepositoryOperation;
};

export type OperationContextValue = {
  operations: SessionOperation[];
  register: (admission: ProjectIndexAdmission, requestId: string) => void;
  update: (operationId: string, status: RepositoryOperation) => void;
  dismiss: (operationId: string) => void;
};

export const OperationContext = createContext<OperationContextValue | undefined>(undefined);

export function useOperations(): OperationContextValue {
  const value = useContext(OperationContext);
  if (value === undefined) {
    throw new Error("Operation context is unavailable");
  }
  return value;
}
