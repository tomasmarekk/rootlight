// Defines the durable browser routes for Rootlight's primary product surfaces.

import { createBrowserRouter, Navigate, RouterProvider } from "react-router";

import { AppShell } from "./shell/app-shell";
import { DiagnosticsPage } from "./views/diagnostics-page";
import { NotFoundPage } from "./views/not-found-page";
import { OperationsPage } from "./views/operations-page";
import { ProjectWorkspacePage } from "./views/project-workspace-page";
import { ProjectsPage } from "./views/projects-page";

const router = createBrowserRouter([
  {
    path: "/",
    element: <AppShell />,
    children: [
      {
        index: true,
        element: <Navigate replace to="/projects" />,
      },
      {
        path: "projects",
        element: <ProjectsPage />,
      },
      {
        path: "projects/:repositoryId",
        element: <ProjectWorkspacePage />,
      },
      {
        path: "operations",
        element: <OperationsPage />,
      },
      {
        path: "diagnostics",
        element: <DiagnosticsPage />,
      },
      {
        path: "*",
        element: <NotFoundPage />,
      },
    ],
  },
]);

export function RootlightRouter() {
  return <RouterProvider router={router} />;
}
