// Supplies the narrow D3 declarations referenced by the published Cosmos 3.4 type bundle.
// Rootlight does not call these transitive APIs directly, so only their public shapes are retained.

declare module "d3-zoom" {
  export type D3ZoomEvent<ThisElement, Datum> = {
    sourceEvent: Event | null;
    target: unknown;
    transform: ZoomTransform;
    type: string;
    readonly __thisElement?: ThisElement;
    readonly __datum?: Datum;
  };

  export type ZoomTransform = {
    x: number;
    y: number;
    k: number;
  };

  export type ZoomBehavior<ThisElement, Datum> = {
    readonly __thisElement?: ThisElement;
    readonly __datum?: Datum;
  };
}

declare module "d3-drag" {
  export type SubjectPosition = {
    x: number;
    y: number;
  };

  export type D3DragEvent<ThisElement, Datum, Subject> = {
    sourceEvent: Event;
    subject: Subject;
    target: unknown;
    type: string;
    x: number;
    y: number;
    readonly __thisElement?: ThisElement;
    readonly __datum?: Datum;
  };

  export type DragBehavior<ThisElement, Datum, Subject> = {
    readonly __thisElement?: ThisElement;
    readonly __datum?: Datum;
    readonly __subject?: Subject;
  };
}
