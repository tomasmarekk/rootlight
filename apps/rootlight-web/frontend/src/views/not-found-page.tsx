// Returns users from unknown client routes without leaking route input.

import { Button } from "@heroui/react/button";
import { useNavigate } from "react-router";

export function NotFoundPage() {
  const navigate = useNavigate();
  return (
    <div className="content-container">
      <section className="quiet-panel">
        <p className="eyebrow">Unknown view</p>
        <h1>This Rootlight route does not exist</h1>
        <p>Return to the local repository catalog to continue.</p>
        <Button variant="primary" onPress={() => navigate("/projects")}>
          Open projects
        </Button>
      </section>
    </div>
  );
}
