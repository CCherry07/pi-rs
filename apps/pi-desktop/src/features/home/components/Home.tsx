import { useTranslation } from 'react-i18next'
import { HomeActions } from './HomeActions'
import { HomeLatestAgentsSection } from './HomeLatestAgentsSection'
import type { LatestAgentRun } from '../homeTypes'

type HomeProps = {
  onAddWorkspace: () => void
  onAddWorkspaceFromUrl: () => void
  latestAgentRuns: LatestAgentRun[]
  isLoadingLatestAgents: boolean
  onSelectThread: (workspaceId: string, threadId: string) => void
}

export function Home({ onAddWorkspace, onAddWorkspaceFromUrl, latestAgentRuns, isLoadingLatestAgents, onSelectThread }: HomeProps) {
  const { t } = useTranslation('home')
  return (
    <div className="home">
      <div className="home-hero">
        <div className="home-title">Pi</div>
        <div className="home-subtitle">{t('hero.subtitle')}</div>
      </div>
      <HomeLatestAgentsSection latestAgentRuns={latestAgentRuns} isLoadingLatestAgents={isLoadingLatestAgents} onSelectThread={onSelectThread} />
      <HomeActions onAddWorkspace={onAddWorkspace} onAddWorkspaceFromUrl={onAddWorkspaceFromUrl} />
    </div>
  )
}
