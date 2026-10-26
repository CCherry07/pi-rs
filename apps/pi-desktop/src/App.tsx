import { lazy, Suspense } from "react";
import "./styles/app.css";
import { useWindowLabel } from "@/features/layout/hooks/useWindowLabel";
import MainApp from "@app/components/MainApp";

const AboutView = lazy(() =>
  import("@/features/about/components/AboutView").then((module) => ({
    default: module.AboutView,
  })),
);

export default function App() {
  const windowLabel = useWindowLabel();

  if (windowLabel === "about") {
    return (
      <Suspense fallback={null}>
        <AboutView />
      </Suspense>
    );
  }

  return <MainApp />;
}
