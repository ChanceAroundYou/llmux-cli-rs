import React, { useState, useEffect, useMemo } from 'react';

// ==========================================
// 1. 矢量图标库 (SVG Icons - 保证无外部依赖加载失败问题)
// ==========================================
const Icons = {
  Users: () => (
    <svg className="w-5 h-5 text-blue-600" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M17 20h5v-2a3 3 0 00-5.356-1.857M17 20H7m10 0v-2c0-.656-.126-1.283-.356-1.857M7 20H2v-2a3 3 0 015.356-1.857M7 20v-2c0-.656.126-1.283.356-1.857m0 0a5.002 5.002 0 019.288 0M15 7a3 3 0 11-6 0 3 3 0 016 0zm6 3a2 2 0 11-4 0 2 2 0 014 0zM7 10a2 2 0 11-4 0 2 2 0 014 0z" />
    </svg>
  ),
  Zap: () => (
    <svg className="w-5 h-5 text-amber-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M13 10V3L4 14h7v7l9-11h-7z" />
    </svg>
  ),
  Key: () => (
    <svg className="w-5 h-5 text-purple-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M15 7a2 2 0 012 2m-2 2a2 2 0 002-2m0 0a2 2 0 00-2-2m0 0a2 2 0 00-2 2m0 10V15m0 0V13m0 0a4 4 0 10-4-4v12a2 2 0 002 2h4a2 2 0 002-2v-4a2 2 0 00-2-2zm-2-4h.01" />
    </svg>
  ),
  Shield: () => (
    <svg className="w-5 h-5 text-emerald-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z" />
    </svg>
  ),
  Refresh: ({ className = "" }) => (
    <svg className={`w-4 h-4 ${className}`} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4 4v5h.582m15.356 2A8.001 8.001 0 1121.21 12H19c0 .73-.13 1.43-.36 2.083M12 4a8.001 8.001 0 00-7.21 4.79H2v-2" />
    </svg>
  ),
  Plus: () => (
    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M12 4v16m8-8H4" />
    </svg>
  ),
  Search: () => (
    <svg className="w-4 h-4 text-slate-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
    </svg>
  ),
  CheckCircle: () => (
    <svg className="w-4 h-4 text-emerald-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
    </svg>
  ),
  AlertTriangle: ({ className = "w-4 h-4" }) => (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
    </svg>
  ),
  X: () => (
    <svg className="w-5 h-5 text-slate-400 hover:text-slate-600 cursor-pointer" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
    </svg>
  ),
  ChevronRight: ({ className = "w-4 h-4" }) => (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9 5l7 7-7 7" />
    </svg>
  ),
  Activity: () => (
    <svg className="w-4 h-4 text-blue-600" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9 19v-6a2 2 0 00-2-2H5a2 2 0 00-2 2v6a2 2 0 002 2h2a2 2 0 002-2zm0 0V9a2 2 0 012-2h2a2 2 0 012 2v10m-6 0a2 2 0 002 2h2a2 2 0 002-2m0 0V5a2 2 0 012-2h2a2 2 0 012 2v14a2 2 0 002 2h2a2 2 0 002-2z" />
    </svg>
  ),
  Database: () => (
    <svg className="w-4 h-4 text-slate-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4m0 5c0 2.21-3.582 4-8 4s-8-1.79-8-4" />
    </svg>
  )
};

// ==========================================
// 2. 初始模拟数据集
// ==========================================
const INITIAL_ALIASES = [
  { id: '1', name: 'deepseek-v4-flash', type: 'deepseek-v4-flash', latency: 0.1, status: 'healthy', provider: 'DeepSeek', region: '上海一区', history: [0.12, 0.08, 0.15, 0.09, 0.11, 0.10, 0.10] },
  { id: '2', name: 'mimo2.5-p', type: 'mimo-v2.5-pro', latency: 1.4, status: 'healthy', provider: 'MimoAI', region: '北京二区', history: [1.2, 1.5, 1.3, 1.6, 1.4, 1.4, 1.45] },
  { id: '3', name: 'mm', type: 'minimax-m2.7', latency: 1.1, status: 'healthy', provider: 'MiniMax', region: '广州三区', history: [0.9, 1.2, 1.1, 1.0, 1.3, 1.1, 1.12] },
  { id: '4', name: 'doubao', type: 'doubao-seed-code', latency: 1.0, status: 'healthy', provider: '字节火山引擎', region: '杭州四区', history: [0.95, 1.1, 1.05, 1.0, 0.98, 1.02, 1.0] },
  { id: '5', name: 'gpt-4o-proxy', type: 'openai-gpt-4o', latency: 1.8, status: 'healthy', provider: 'OpenAI', region: '美国东部', history: [1.6, 1.9, 1.75, 1.82, 1.88, 1.79, 1.8] },
  { id: '6', name: 'claude-3-haiku-hub', type: 'anthropic-claude-3-haiku', latency: 0.8, status: 'healthy', provider: 'Anthropic', region: '美国西部', history: [0.75, 0.82, 0.9, 0.85, 0.78, 0.81, 0.8] },
  { id: '7', name: 'gemini-flash-node', type: 'google-gemini-2.5-flash', latency: 0.6, status: 'healthy', provider: 'Google', region: '香港节点', history: [0.55, 0.62, 0.7, 0.65, 0.58, 0.61, 0.6] }
];

const INITIAL_LOGS = [
  { id: 101, timestamp: '14:32:05', alias: 'deepseek-v4-flash', method: 'POST', path: '/v1/chat/completions', status: 200, latency: '0.12s', error: false },
  { id: 102, timestamp: '14:31:48', alias: 'mimo2.5-p', method: 'POST', path: '/v1/embeddings', status: 200, latency: '1.45s', error: false },
  { id: 103, timestamp: '14:31:12', alias: 'mm', method: 'POST', path: '/v1/chat/completions', status: 200, latency: '1.12s', error: false },
  { id: 104, timestamp: '14:30:55', alias: 'doubao', method: 'POST', path: '/v1/chat/completions', status: 502, latency: '1.02s', error: true, errorMsg: 'Bad Gateway (Service Temporarily Overloaded)' },
];

export default function App() {
  // ==========================================
  // 3. 状态管理
  // ==========================================
  const [aliases, setAliases] = useState(INITIAL_ALIASES);
  const [logs, setLogs] = useState(INITIAL_LOGS);
  const [searchQuery, setSearchQuery] = useState('');
  const [selectedProvider, setSelectedProvider] = useState('All');
  const [selectedAliasId, setSelectedAliasId] = useState(null);
  
  // 日志和分析控制
  const [onlyShowErrors, setOnlyShowErrors] = useState(false);
  const [logViewTab, setLogViewTab] = useState('live'); // 'live' | 'analytics'
  const [isRefreshing, setIsRefreshing] = useState(false);

  // 快速连接 Modal
  const [isConnectOpen, setIsConnectOpen] = useState(false);
  const [newAccount, setNewAccount] = useState({ provider: 'DeepSeek', aliasName: '', apiKey: '', endpoint: 'https://api.deepseek.com/v1' });

  // 账户和全局基本计数
  const [stats, setStats] = useState({
    totalAccounts: 3,
    totalAliases: 7,
    apiKeyCount: 1,
    healthyAccounts: 3
  });

  // ==========================================
  // 4. 实时流量日志模拟引擎
  // ==========================================
  useEffect(() => {
    const interval = setInterval(() => {
      // 随机选取一个别名产生新调用
      const randomAlias = aliases[Math.floor(Math.random() * aliases.length)];
      
      // 模拟偶然发生 4% 概率错误，96% 成功率
      const isError = Math.random() < 0.04;
      const responseStatus = isError ? [502, 504, 429][Math.floor(Math.random() * 3)] : 200;
      const currentLatencyVal = (randomAlias.latency + (Math.random() * 0.3 - 0.15)).toFixed(2);
      
      const newLog = {
        id: Date.now(),
        timestamp: new Date().toLocaleTimeString('zh-CN', { hour12: false }),
        alias: randomAlias.name,
        method: Math.random() > 0.3 ? 'POST' : 'GET',
        path: Math.random() > 0.5 ? '/v1/chat/completions' : '/v1/models',
        status: responseStatus,
        latency: `${Math.max(0.05, parseFloat(currentLatencyVal))}s`,
        error: isError,
        errorMsg: isError ? 'Gateway Request Timeout / Limit Exceeded' : undefined
      };

      setLogs(prev => [newLog, ...prev.slice(0, 19)]); // 保留最近 20 条日志

      // 动态更新对应别名的最近历史折线值
      setAliases(prevAliases => prevAliases.map(item => {
        if (item.id === randomAlias.id) {
          const updatedHistory = [...item.history.slice(1), parseFloat(currentLatencyVal)];
          const avgLatency = (updatedHistory.reduce((a, b) => a + b, 0) / updatedHistory.length).toFixed(2);
          return {
            ...item,
            latency: Math.max(0.05, parseFloat(avgLatency)),
            history: updatedHistory
          };
        }
        return item;
      }));

    }, 4500); // 每 4.5s 涌入一个新请求，营造生产网关环境的律动感

    return () => clearInterval(interval);
  }, [aliases]);

  // 计算当前的延迟平均值和成功率指标
  const calculatedMetrics = useMemo(() => {
    if (logs.length === 0) return { successRate: '100%', avgLatency: '0.0s' };
    const successCount = logs.filter(l => !l.error).length;
    const totalCount = logs.length;
    const successRate = `${Math.round((successCount / totalCount) * 100)}%`;
    
    const totalLatencies = logs.map(l => parseFloat(l.latency));
    const avgLatency = `${(totalLatencies.reduce((a, b) => a + b, 0) / totalLatencies.length).toFixed(2)}s`;
    
    return { successRate, avgLatency };
  }, [logs]);

  // ==========================================
  // 5. 交互行为
  // ==========================================
  // 刷新所有模型节点动作
  const handleRefresh = () => {
    setIsRefreshing(true);
    setTimeout(() => {
      // 随机抖动一下延迟值以证明正在重新评测网络
      setAliases(prev => prev.map(item => {
        const jitter = (Math.random() * 0.2 - 0.1);
        const newLat = Math.max(0.04, parseFloat((item.latency + jitter).toFixed(2)));
        return {
          ...item,
          latency: newLat,
          history: [...item.history.slice(1), newLat]
        };
      }));
      setIsRefreshing(false);
    }, 1200);
  };

  // 添加新连接
  const handleAddConnect = (e) => {
    e.preventDefault();
    if (!newAccount.aliasName) return;

    const newId = (aliases.length + 1).toString();
    const newAliasNode = {
      id: newId,
      name: newAccount.aliasName,
      type: `${newAccount.provider.toLowerCase()}-node`,
      latency: 0.15,
      status: 'healthy',
      provider: newAccount.provider,
      region: '新建配置节点',
      history: [0.15, 0.15, 0.15, 0.15, 0.15, 0.15, 0.15]
    };

    setAliases(prev => [...prev, newAliasNode]);
    setStats(prev => ({
      ...prev,
      totalAliases: prev.totalAliases + 1,
      totalAccounts: prev.totalAccounts + 1,
      apiKeyCount: prev.apiKeyCount + 1,
      healthyAccounts: prev.healthyAccounts + 1
    }));
    setIsConnectOpen(false);
    // 重置表单
    setNewAccount({ provider: 'DeepSeek', aliasName: '', apiKey: '', endpoint: 'https://api.deepseek.com/v1' });
  };

  // 过滤别名节点
  const filteredAliases = useMemo(() => {
    return aliases.filter(item => {
      const matchSearch = item.name.toLowerCase().includes(searchQuery.toLowerCase()) || 
                          item.type.toLowerCase().includes(searchQuery.toLowerCase());
      const matchProvider = selectedProvider === 'All' || item.provider === selectedProvider;
      return matchSearch && matchProvider;
    });
  }, [aliases, searchQuery, selectedProvider]);

  // 过滤日志明细
  const filteredLogs = useMemo(() => {
    if (onlyShowErrors) {
      return logs.filter(l => l.error);
    }
    return logs;
  }, [logs, onlyShowErrors]);

  const providerList = ['All', 'DeepSeek', 'MimoAI', 'MiniMax', '字节火山引擎', 'OpenAI', 'Anthropic', 'Google'];

  return (
    <div className="min-h-screen bg-slate-50 text-slate-800 font-sans antialiased transition-colors duration-300">
      
      {/* ==========================================
          页眉区域 (Header Area)
         ========================================== */}
      <header className="border-b border-slate-200 bg-white sticky top-0 z-40 shadow-sm backdrop-blur-md bg-white/95">
        <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 py-4 flex flex-col sm:flex-row justify-between items-start sm:items-center gap-4">
          <div>
            <div className="flex items-center gap-2.5">
              <span className="h-3 w-3 rounded-full bg-blue-600 animate-pulse"></span>
              <h1 className="text-2xl font-bold tracking-tight text-slate-900">AI 基础设施网关仪表盘</h1>
            </div>
            <p className="text-sm text-slate-500 mt-1">
              实时安全审计、健康检测及多云多账号多端点路由的网关系统
            </p>
          </div>
          
          {/* 右侧主交互按钮 */}
          <div className="flex items-center gap-3 w-full sm:w-auto">
            <button 
              onClick={handleRefresh}
              className="flex-1 sm:flex-initial flex items-center justify-center gap-2 px-4 py-2 border border-slate-200 bg-white hover:bg-slate-50 active:bg-slate-100 text-slate-700 text-sm font-semibold rounded-lg shadow-sm transition-all duration-200"
            >
              <Icons.Refresh className={isRefreshing ? 'animate-spin text-blue-600' : 'text-slate-500'} />
              <span>{isRefreshing ? '刷新中...' : '刷新模型库'}</span>
            </button>
            <button 
              onClick={() => setIsConnectOpen(true)}
              className="flex-1 sm:flex-initial flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-semibold rounded-lg shadow-md hover:shadow-lg active:scale-95 transition-all duration-200"
            >
              <Icons.Plus />
              <span>快速连接</span>
            </button>
          </div>
        </div>
      </header>

      {/* ==========================================
          主容器布局 (Main Layout Container)
         ========================================== */}
      <main className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 py-8 space-y-8">
        
        {/* 顶部四指标看板卡片组 */}
        <section className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-6">
          
          {/* 指标卡片 1 - 账户总数 */}
          <div className="bg-white border border-slate-200 p-5 rounded-2xl shadow-sm hover:shadow-md transition-shadow duration-300 relative overflow-hidden group">
            <div className="absolute top-0 right-0 h-16 w-16 bg-blue-50 rounded-bl-full -z-10 group-hover:scale-110 transition-transform duration-300"></div>
            <div className="flex justify-between items-start">
              <div>
                <p className="text-sm font-medium text-slate-400">账户总数</p>
                <h3 className="text-3xl font-extrabold text-slate-900 mt-2 tracking-tight">
                  {stats.totalAccounts}
                </h3>
              </div>
              <div className="p-3 bg-blue-100 rounded-xl">
                <Icons.Users />
              </div>
            </div>
            <div className="mt-4 flex items-center text-xs text-blue-600 font-semibold gap-1">
              <span>活动中: {stats.totalAccounts} 个云提供商</span>
            </div>
          </div>

          {/* 指标卡片 2 - 别名总数 */}
          <div className="bg-white border border-slate-200 p-5 rounded-2xl shadow-sm hover:shadow-md transition-shadow duration-300 relative overflow-hidden group">
            <div className="absolute top-0 right-0 h-16 w-16 bg-amber-50 rounded-bl-full -z-10 group-hover:scale-110 transition-transform duration-300"></div>
            <div className="flex justify-between items-start">
              <div>
                <p className="text-sm font-medium text-slate-400">别名总数</p>
                <h3 className="text-3xl font-extrabold text-slate-900 mt-2 tracking-tight">
                  {stats.totalAliases}
                </h3>
              </div>
              <div className="p-3 bg-amber-100 rounded-xl">
                <Icons.Zap />
              </div>
            </div>
            <div className="mt-4 flex items-center text-xs text-amber-600 font-semibold gap-1">
              <span>映射端点: {stats.totalAliases} 个网关别名</span>
            </div>
          </div>

          {/* 指标卡片 3 - API KEY 数量 */}
          <div className="bg-white border border-slate-200 p-5 rounded-2xl shadow-sm hover:shadow-md transition-shadow duration-300 relative overflow-hidden group">
            <div className="absolute top-0 right-0 h-16 w-16 bg-purple-50 rounded-bl-full -z-10 group-hover:scale-110 transition-transform duration-300"></div>
            <div className="flex justify-between items-start">
              <div>
                <p className="text-sm font-medium text-slate-400">API KEY 凭证</p>
                <h3 className="text-3xl font-extrabold text-slate-900 mt-2 tracking-tight">
                  {stats.apiKeyCount}
                </h3>
              </div>
              <div className="p-3 bg-purple-100 rounded-xl">
                <Icons.Key />
              </div>
            </div>
            <div className="mt-4 flex items-center text-xs text-purple-600 font-semibold gap-1">
              <span>状态: 已全部加密托管</span>
            </div>
          </div>

          {/* 指标卡片 4 - 健康账户数 */}
          <div className="bg-white border border-slate-200 p-5 rounded-2xl shadow-sm hover:shadow-md transition-shadow duration-300 relative overflow-hidden group">
            <div className="absolute top-0 right-0 h-16 w-16 bg-emerald-50 rounded-bl-full -z-10 group-hover:scale-110 transition-transform duration-300"></div>
            <div className="flex justify-between items-start">
              <div>
                <p className="text-sm font-medium text-slate-400">健康网关节点</p>
                <h3 className="text-3xl font-extrabold text-slate-900 mt-2 tracking-tight">
                  {stats.healthyAccounts}
                </h3>
              </div>
              <div className="p-3 bg-emerald-100 rounded-xl">
                <Icons.Shield />
              </div>
            </div>
            <div className="mt-4 flex items-center text-xs text-emerald-600 font-semibold gap-1">
              <span>健康状态: 100% 畅通在线</span>
            </div>
          </div>

        </section>

        {/* ==========================================
            双栏控制大盘 (Two Column Details Panel)
           ========================================== */}
        <div className="grid grid-cols-1 lg:grid-cols-12 gap-8 items-start">
          
          {/* 左栏 (占 7 列)：网关别名健康监控 + 精细过滤 */}
          <section className="lg:col-span-7 bg-white border border-slate-200 rounded-2xl shadow-sm overflow-hidden flex flex-col h-full">
            
            {/* 顶栏信息 */}
            <div className="p-6 border-b border-slate-100">
              <div className="flex justify-between items-center flex-wrap gap-2">
                <div className="flex items-center gap-2">
                  <Icons.Database />
                  <h2 className="text-lg font-bold text-slate-900">别名健康概览</h2>
                </div>
                <span className="text-xs bg-emerald-50 text-emerald-700 px-2.5 py-1 rounded-full font-semibold border border-emerald-200">
                  {filteredAliases.length}/{aliases.length} 正常运营中
                </span>
              </div>
              
              {/* 搜索与分类微型过滤器组件 */}
              <div className="mt-4 space-y-3">
                <div className="relative">
                  <div className="absolute inset-y-0 left-0 pl-3 flex items-center pointer-events-none">
                    <Icons.Search />
                  </div>
                  <input
                    type="text"
                    placeholder="输入别名或模型类型以快速筛选..."
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                    className="block w-full pl-9 pr-4 py-2 border border-slate-200 rounded-lg text-sm bg-slate-50 focus:bg-white focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 transition-all duration-200"
                  />
                </div>

                {/* 渠道过滤药丸标签 */}
                <div className="flex items-center gap-1.5 overflow-x-auto py-1 no-scrollbar text-xs">
                  <span className="text-slate-400 shrink-0 font-medium mr-1">厂商过滤:</span>
                  {providerList.map(prov => (
                    <button
                      key={prov}
                      onClick={() => setSelectedProvider(prov)}
                      className={`px-2.5 py-1 rounded-md font-semibold transition-all duration-150 shrink-0 ${
                        selectedProvider === prov 
                          ? 'bg-blue-600 text-white shadow-sm' 
                          : 'bg-slate-100 text-slate-600 hover:bg-slate-200'
                      }`}
                    >
                      {prov === 'All' ? '全部' : prov}
                    </button>
                  ))}
                </div>
              </div>
            </div>

            {/* 别名网格节点详情 */}
            <div className="divide-y divide-slate-100 max-h-[580px] overflow-y-auto custom-scrollbar">
              {filteredAliases.length === 0 ? (
                <div className="p-12 text-center text-slate-400 text-sm">
                  没有找到符合过滤条件的网关别名节点
                </div>
              ) : (
                filteredAliases.map((item) => {
                  const isExpanded = selectedAliasId === item.id;
                  return (
                    <div 
                      key={item.id} 
                      className={`hover:bg-slate-50/50 transition-colors duration-200 ${isExpanded ? 'bg-slate-50/70' : ''}`}
                    >
                      {/* 点击可折叠明细 */}
                      <div 
                        onClick={() => setSelectedAliasId(isExpanded ? null : item.id)}
                        className="p-5 flex items-center justify-between gap-4 cursor-pointer"
                      >
                        <div className="flex items-center gap-4">
                          {/* 健康跳动指示点 */}
                          <div className="relative flex h-3 w-3">
                            <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75"></span>
                            <span className="relative inline-flex rounded-full h-3 w-3 bg-emerald-500"></span>
                          </div>
                          
                          <div>
                            <div className="flex items-center gap-2 flex-wrap">
                              <span className="font-bold text-slate-900 tracking-tight">{item.name}</span>
                              <span className="text-[10px] bg-slate-100 text-slate-500 font-semibold px-2 py-0.5 rounded border border-slate-200">
                                {item.provider}
                              </span>
                            </div>
                            <p className="text-xs text-slate-400 font-mono mt-1">{item.type}</p>
                          </div>
                        </div>

                        {/* 中间微型 Sparkline (简易 SVG 折线图，展示其延迟波动趋势) */}
                        <div className="hidden sm:block w-24 h-10">
                          <svg className="w-full h-full overflow-visible" viewBox="0 0 100 40">
                            <defs>
                              <linearGradient id={`grad-${item.id}`} x1="0" y1="0" x2="0" y2="1">
                                <stop offset="0%" stopColor="#2563eb" stopOpacity="0.2"/>
                                <stop offset="100%" stopColor="#2563eb" stopOpacity="0"/>
                              </linearGradient>
                            </defs>
                            {/* 闭合面积填充 */}
                            <path
                              d={`M 0,40 ${item.history.map((val, idx) => `L ${idx * 16.6},${40 - (val / 2.5) * 40}`).join(' ')} L 100,40 Z`}
                              fill={`url(#grad-${item.id})`}
                              stroke="none"
                            />
                            {/* 主趋势线条 */}
                            <path
                              d={item.history.map((val, idx) => `${idx === 0 ? 'M' : 'L'} ${idx * 16.6},${40 - (val / 2.5) * 40}`).join(' ')}
                              fill="none"
                              stroke="#3b82f6"
                              strokeWidth="2"
                              strokeLinecap="round"
                              strokeLinejoin="round"
                            />
                            {/* 最后一测的闪红亮点 */}
                            <circle cx="100" cy={40 - (item.history[item.history.length-1] / 2.5) * 40} r="3" fill="#10b981" />
                          </svg>
                        </div>

                        {/* 右侧延迟信息 */}
                        <div className="flex items-center gap-3">
                          <div className="text-right">
                            <span className={`text-sm font-bold font-mono px-2 py-1 rounded ${
                              item.latency < 0.5 ? 'text-emerald-600 bg-emerald-50' : 
                              item.latency < 1.2 ? 'text-amber-600 bg-amber-50' : 'text-blue-600 bg-blue-50'
                            }`}>
                              {item.latency.toFixed(1)}s
                            </span>
                            <p className="text-[10px] text-slate-400 mt-1">瞬时延迟</p>
                          </div>
                          <Icons.ChevronRight className={`transform transition-transform duration-200 ${isExpanded ? 'rotate-90 text-blue-600' : 'text-slate-400'}`} />
                        </div>
                      </div>

                      {/* 展开的微服务拓扑和 API 节点细节 */}
                      {isExpanded && (
                        <div className="px-5 pb-5 pt-1 border-t border-slate-100 bg-slate-50/40 text-xs text-slate-600 grid grid-cols-1 md:grid-cols-2 gap-4">
                          <div className="space-y-2">
                            <div>
                              <span className="text-slate-400 block font-medium">路由区域：</span>
                              <span className="font-semibold text-slate-700">{item.region}</span>
                            </div>
                            <div>
                              <span className="text-slate-400 block font-medium">路由端点 Endpoint：</span>
                              <span className="font-mono text-[11px] text-slate-500 break-all bg-slate-100 p-1 rounded block mt-0.5">
                                https://api.gateway.cloud/v1/router/{item.name}/chat/completions
                              </span>
                            </div>
                          </div>
                          <div className="space-y-2">
                            <div>
                              <span className="text-slate-400 block font-medium">历史检测窗口平均波幅：</span>
                              <div className="flex gap-1 mt-1 font-mono text-[10px] text-slate-500">
                                {item.history.map((h, i) => (
                                  <span key={i} className="bg-slate-200/60 px-1 py-0.5 rounded">{h}s</span>
                                ))}
                              </div>
                            </div>
                            <div className="flex justify-between items-center pt-2">
                              <div>
                                <span className="text-slate-400 block font-medium">限流(Rate Limit)：</span>
                                <span className="text-emerald-600 font-semibold">10,000 RPM (正常)</span>
                              </div>
                              <button className="px-3 py-1 bg-white border border-slate-200 text-slate-700 hover:bg-slate-50 font-semibold rounded shadow-sm">
                                强制排载
                              </button>
                            </div>
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })
              )}
            </div>

            {/* 底部导航提示 */}
            <div className="p-4 bg-slate-50 border-t border-slate-100 text-[11px] text-slate-400 flex justify-between">
              <span>* 节点每 30 秒进行一次高保真 Ping 心跳审计</span>
              <span>数据源：本地 API 网关</span>
            </div>
          </section>

          {/* 右栏 (占 5 列)：智能日志监控动态 + 性能透视图表 */}
          <section className="lg:col-span-5 bg-white border border-slate-200 rounded-2xl shadow-sm overflow-hidden flex flex-col h-full">
            
            {/* 顶栏控制组 */}
            <div className="p-6 border-b border-slate-100">
              <div className="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-3">
                
                {/* 日志和分析的 Tab 切换 */}
                <div className="flex bg-slate-100 p-1 rounded-lg">
                  <button
                    onClick={() => setLogViewTab('live')}
                    className={`px-3 py-1 text-xs font-semibold rounded-md transition-all ${
                      logViewTab === 'live' 
                        ? 'bg-white text-slate-950 shadow-sm' 
                        : 'text-slate-500 hover:text-slate-900'
                    }`}
                  >
                    实时日志 (Live Logs)
                  </button>
                  <button
                    onClick={() => setLogViewTab('analytics')}
                    className={`px-3 py-1 text-xs font-semibold rounded-md transition-all ${
                      logViewTab === 'analytics' 
                        ? 'bg-white text-slate-950 shadow-sm' 
                        : 'text-slate-500 hover:text-slate-900'
                    }`}
                  >
                    性能分析 (Analytics)
                  </button>
                </div>

                {/* 只看错误过滤 */}
                {logViewTab === 'live' && (
                  <button
                    onClick={() => setOnlyShowErrors(!onlyShowErrors)}
                    className={`flex items-center gap-1.5 px-3 py-1 rounded-md text-xs font-bold transition-all border ${
                      onlyShowErrors 
                        ? 'bg-rose-50 border-rose-200 text-rose-700 hover:bg-rose-100' 
                        : 'bg-slate-50 border-slate-200 text-slate-500 hover:bg-slate-100'
                    }`}
                  >
                    <Icons.AlertTriangle className="w-3.5 h-3.5" />
                    <span>只看错误</span>
                  </button>
                )}
              </div>

              {/* 核心指标统计透视 (成功率 & 延迟) */}
              <div className="grid grid-cols-2 gap-4 mt-6 p-4 bg-slate-50 rounded-xl border border-slate-100">
                <div>
                  <span className="text-[11px] font-medium text-slate-400 block uppercase tracking-wider">最近成功率</span>
                  <div className="flex items-baseline gap-1 mt-1">
                    <span className="text-xl font-black text-slate-950 tracking-tight">{calculatedMetrics.successRate}</span>
                    <span className="text-[10px] text-emerald-600 font-bold">▲ 0.1%</span>
                  </div>
                </div>
                <div>
                  <span className="text-[11px] font-medium text-slate-400 block uppercase tracking-wider">最近 20 次请求平均延迟</span>
                  <div className="flex items-baseline gap-1 mt-1">
                    <span className="text-xl font-black text-slate-950 tracking-tight">{calculatedMetrics.avgLatency}</span>
                    <span className="text-[10px] text-emerald-600 font-bold">▼ 0.05s</span>
                  </div>
                </div>
              </div>
            </div>

            {/* 根据 Tab 展示不同的内容 */}
            <div className="flex-1 p-6 max-h-[420px] overflow-y-auto custom-scrollbar min-h-[380px]">
              
              {logViewTab === 'live' ? (
                /* ===================
                   子面板一：实时日志流
                   =================== */
                <div className="space-y-3">
                  {filteredLogs.length === 0 ? (
                    <div className="flex flex-col items-center justify-center py-20 text-center text-slate-300">
                      <Icons.AlertTriangle className="w-12 h-12 text-slate-300 mb-2" />
                      <p className="text-sm">近期无符合筛选的活动记录</p>
                    </div>
                  ) : (
                    filteredLogs.map((log) => (
                      <div 
                        key={log.id} 
                        className={`p-3.5 rounded-xl border text-xs font-mono transition-all duration-300 ${
                          log.error 
                            ? 'bg-rose-50/50 border-rose-100 hover:bg-rose-50' 
                            : 'bg-slate-50 border-slate-100 hover:bg-slate-100/50'
                        }`}
                      >
                        <div className="flex justify-between items-start gap-2 flex-wrap">
                          <div className="flex items-center gap-2">
                            <span className="text-slate-400 text-[10px]">{log.timestamp}</span>
                            <span className={`px-1.5 py-0.5 rounded text-[10px] font-bold ${
                              log.method === 'POST' ? 'bg-blue-100 text-blue-700' : 'bg-emerald-100 text-emerald-700'
                            }`}>
                              {log.method}
                            </span>
                            <span className="font-bold text-slate-700">{log.alias}</span>
                          </div>
                          
                          <div className="flex items-center gap-2">
                            <span className="text-slate-500 text-[11px]">{log.latency}</span>
                            <span className={`px-1.5 py-0.5 rounded text-[10px] font-extrabold ${
                              log.status === 200 ? 'bg-emerald-100 text-emerald-800' : 'bg-rose-100 text-rose-800'
                            }`}>
                              {log.status}
                            </span>
                          </div>
                        </div>

                        <div className="mt-1.5 flex justify-between items-center text-[11px] text-slate-400">
                          <span className="truncate max-w-[200px] sm:max-w-xs">{log.path}</span>
                        </div>

                        {log.error && (
                          <div className="mt-2 p-2 bg-rose-100/40 rounded border border-rose-200/30 text-rose-700 text-[10px] leading-relaxed">
                            <strong>[异常日志]</strong> {log.errorMsg}
                          </div>
                        )}
                      </div>
                    ))
                  )}
                </div>
              ) : (
                /* ===================
                   子面板二：可视化透视图表
                   =================== */
                <div className="space-y-6">
                  <div>
                    <h4 className="text-sm font-semibold text-slate-700 mb-3">并发与负载分配趋势</h4>
                    <div className="h-44 w-full bg-slate-50 border border-slate-100 rounded-xl relative p-2 flex items-end justify-between">
                      {/* 用柱状图反映 7 个别名的近端请求负载分布 */}
                      {aliases.map((item, idx) => {
                        const heightPercentage = Math.min(100, Math.max(15, (item.latency * 50)));
                        return (
                          <div key={idx} className="flex flex-col items-center w-8 group">
                            <div className="text-[10px] text-slate-400 font-mono mb-1 scale-90 opacity-0 group-hover:opacity-100 transition-opacity">
                              {item.latency}s
                            </div>
                            <div 
                              className="w-full bg-blue-600 rounded-t-md hover:bg-blue-700 transition-all cursor-pointer relative"
                              style={{ height: `${heightPercentage}px` }}
                            >
                              <div className="absolute inset-0 bg-white/20 animate-pulse rounded-t-md"></div>
                            </div>
                            <div className="text-[9px] text-slate-500 mt-2 truncate w-full text-center scale-90" title={item.name}>
                              {item.name.substring(0, 4)}..
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  </div>

                  <div className="space-y-2">
                    <h4 className="text-sm font-semibold text-slate-700">实时健康评级：A++</h4>
                    <p className="text-xs text-slate-500 leading-relaxed">
                      基于最近 24 小时的全生命周期审计，由于网关自动切换与多重灾备，当前的平均接口无损路由成功率保持在 <strong className="text-emerald-600">99.8%</strong> 以上。
                    </p>
                  </div>
                </div>
              )}

            </div>

            {/* 清空日志或重置动作 */}
            <div className="p-4 bg-slate-50 border-t border-slate-100 flex justify-between items-center text-xs">
              <span className="text-slate-400">滑动面板查看全部 {logs.length} 条网关交互记录</span>
              {logViewTab === 'live' && (
                <button 
                  onClick={() => setLogs([])}
                  className="text-blue-600 hover:text-blue-800 font-semibold"
                >
                  清空日志
                </button>
              )}
            </div>
          </section>

        </div>
      </main>

      {/* ==========================================
          快速连接 (Quick Connect) 模态对话框
         ========================================== */}
      {isConnectOpen && (
        <div className="fixed inset-0 z-50 overflow-y-auto">
          {/* 背景遮罩 */}
          <div 
            className="fixed inset-0 bg-slate-900/60 backdrop-blur-sm transition-opacity"
            onClick={() => setIsConnectOpen(false)}
          ></div>

          {/* 模态弹框内容 */}
          <div className="flex min-h-full items-center justify-center p-4 text-center">
            <div className="relative transform overflow-hidden rounded-2xl bg-white text-left shadow-2xl transition-all w-full max-w-lg p-6 border border-slate-100 animate-in fade-in zoom-in duration-200">
              
              {/* 头部 */}
              <div className="flex justify-between items-center pb-4 border-b border-slate-100">
                <div className="flex items-center gap-2">
                  <div className="p-2 bg-blue-100 text-blue-600 rounded-lg">
                    <Icons.Zap />
                  </div>
                  <div>
                    <h3 className="text-lg font-extrabold text-slate-900">快速连接 AI 基础账户</h3>
                    <p className="text-xs text-slate-400 mt-0.5">对接新的底层模型提供商至网关</p>
                  </div>
                </div>
                <button onClick={() => setIsConnectOpen(false)}>
                  <Icons.X />
                </button>
              </div>

              {/* 连接表单 */}
              <form onSubmit={handleAddConnect} className="mt-5 space-y-4">
                
                {/* 厂商选择 */}
                <div>
                  <label className="block text-xs font-bold text-slate-500 uppercase tracking-wider mb-1.5">
                    1. 服务商 (Provider)
                  </label>
                  <select
                    value={newAccount.provider}
                    onChange={(e) => setNewAccount({...newAccount, provider: e.target.value})}
                    className="block w-full px-3.5 py-2.5 border border-slate-200 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 transition-all"
                  >
                    <option value="DeepSeek">DeepSeek (深度求索)</option>
                    <option value="MimoAI">MimoAI</option>
                    <option value="MiniMax">MiniMax</option>
                    <option value="字节火山引擎">字节火山引擎 (Doubao)</option>
                    <option value="OpenAI">OpenAI</option>
                    <option value="Anthropic">Anthropic</option>
                    <option value="Google">Google Cloud Vertex</option>
                  </select>
                </div>

                {/* 别名输入 */}
                <div>
                  <label className="block text-xs font-bold text-slate-500 uppercase tracking-wider mb-1.5">
                    2. 网关映射别名 (Alias Name)
                  </label>
                  <input
                    type="text"
                    required
                    placeholder="例如: custom-deepseek-speed"
                    value={newAccount.aliasName}
                    onChange={(e) => setNewAccount({...newAccount, aliasName: e.target.value})}
                    className="block w-full px-3.5 py-2.5 border border-slate-200 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 transition-all font-mono"
                  />
                </div>

                {/* API Key */}
                <div>
                  <label className="block text-xs font-bold text-slate-500 uppercase tracking-wider mb-1.5">
                    3. API 凭证 (API KEY)
                  </label>
                  <input
                    type="password"
                    required
                    placeholder="sk-........................"
                    value={newAccount.apiKey}
                    onChange={(e) => setNewAccount({...newAccount, apiKey: e.target.value})}
                    className="block w-full px-3.5 py-2.5 border border-slate-200 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 transition-all font-mono"
                  />
                </div>

                {/* 端点 Endpoint */}
                <div>
                  <label className="block text-xs font-bold text-slate-500 uppercase tracking-wider mb-1.5">
                    4. 服务源端点 (Base Endpoint)
                  </label>
                  <input
                    type="text"
                    placeholder="https://api.gateway.com/v1"
                    value={newAccount.endpoint}
                    onChange={(e) => setNewAccount({...newAccount, endpoint: e.target.value})}
                    className="block w-full px-3.5 py-2.5 border border-slate-200 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 transition-all font-mono text-slate-500 bg-slate-50"
                  />
                </div>

                {/* 提示 */}
                <div className="flex gap-2 p-3 bg-blue-50 border border-blue-100 rounded-xl text-[11px] text-blue-800 leading-relaxed">
                  <Icons.CheckCircle />
                  <span>新建成功后，该网关将自动完成一轮 Ping 耗时测定，并生成安全防护令牌与独立的沙箱环境。</span>
                </div>

                {/* 操作按钮 */}
                <div className="pt-4 flex gap-3 justify-end">
                  <button
                    type="button"
                    onClick={() => setIsConnectOpen(false)}
                    className="px-4 py-2 text-xs font-bold text-slate-600 hover:bg-slate-100 rounded-lg transition-colors"
                  >
                    取消
                  </button>
                  <button
                    type="submit"
                    className="px-5 py-2 text-xs font-bold text-white bg-blue-600 hover:bg-blue-700 rounded-lg shadow transition-all duration-200"
                  >
                    测试并接入网关
                  </button>
                </div>

              </form>

            </div>
          </div>
        </div>
      )}

    </div>
  );
}