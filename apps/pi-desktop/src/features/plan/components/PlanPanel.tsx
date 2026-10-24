import type { TurnPlan } from "../../../types";
import { useTranslation } from "react-i18next";
import Check from "lucide-react/dist/esm/icons/check";
import Circle from "lucide-react/dist/esm/icons/circle";
import CircleDot from "lucide-react/dist/esm/icons/circle-dot";

type PlanPanelProps = {
  plan: TurnPlan | null;
  isProcessing: boolean;
};

function formatProgress(plan: TurnPlan) {
  const total = plan.steps.length;
  if (!total) {
    return "";
  }
  const completed = plan.steps.filter((step) => step.status === "completed").length;
  return `${completed}/${total}`;
}

function statusIcon(status: TurnPlan["steps"][number]["status"]) {
  if (status === "completed") {
    return <Check size={14} />;
  }
  if (status === "inProgress") {
    return <CircleDot size={14} />;
  }
  return <Circle size={14} />;
}

export function PlanPanel({ plan, isProcessing }: PlanPanelProps) {
  const { t } = useTranslation("messages");
  const progress = plan ? formatProgress(plan) : "";
  const steps = plan?.steps ?? [];
  const showEmpty = !steps.length && !plan?.explanation;
  const emptyLabel = isProcessing ? t("plan.waiting") : t("plan.empty");

  return (
    <aside className="plan-panel">
      <div className="plan-header">
        <span>{t("plan.title")}</span>
        {progress && <span className="plan-progress">{progress}</span>}
      </div>
      {plan?.explanation && (
        <div className="plan-explanation">{plan.explanation}</div>
      )}
      {showEmpty ? (
        <div className="plan-empty">{emptyLabel}</div>
      ) : (
        <ol className="plan-list">
          {steps.map((step, index) => (
            <li key={`${step.step}-${index}`} className={`plan-step ${step.status}`}>
              <span className="plan-step-status" aria-hidden>
                {statusIcon(step.status)}
              </span>
              <span className="plan-step-text">{step.step}</span>
            </li>
          ))}
        </ol>
      )}
    </aside>
  );
}
