export type ClientId = 'codex' | 'claudeCode';
export type WorkspacePage = 'accounts' | 'service' | 'logs';
export type SourceMode = 'subscription' | 'api';
export type ApiProtocol = 'responses' | 'messages' | 'chatCompletions';
export type ApiNetwork = 'systemProxy' | 'direct';

export type CheckResult = {
  status: 'unknown' | 'checking' | 'passed' | 'failed';
  at: number | null;
  message: string;
};

export type SourceInput = {
  id: string | null;
  name: string;
  protocol: ApiProtocol;
  baseUrl: string;
  model: string;
  network: ApiNetwork;
  enabled: boolean;
  apiKey: string | null;
};

export type ApiSource = Omit<SourceInput, 'id' | 'apiKey'> & {
  id: string;
  hasKey: boolean;
  revision: number;
  verification?: {
    basic?: CheckResult;
    test?: CheckResult;
    stream?: CheckResult;
    tools?: CheckResult;
  };
};

export type ClientStatus = {
  client: ClientId;
  mode: 'off' | 'starting' | 'running' | 'stopping' | 'recoveryRequired' | 'failed';
  sourceId?: string | null;
  sourceName?: string | null;
  sourceRevision?: number | null;
  phase?: string | null;
  error?: string | null;
  activeRequests: number;
  desktopAcceptance?: 'pending';
  baseUrl?: string | null;
  historyIncomplete?: boolean;
  retryVerified?: boolean;
};

export type ApiRequestLog = {
  id: string;
  at: number;
  client: ClientId;
  sourceId: string;
  sourceRevision: number;
  operation: string;
  model: string | null;
  responseModel?: string | null;
  status: string;
  upstreamStatus: number | null;
  elapsedMs: number;
  requestBytes: number;
  responseBytes: number;
  toolCallObserved: boolean;
  usagePartial?: boolean;
  usage?: {
    inputTokens?: number | null;
    outputTokens?: number | null;
    totalTokens?: number | null;
    cachedTokens?: number | null;
    reasoningTokens?: number | null;
  } | null;
};
