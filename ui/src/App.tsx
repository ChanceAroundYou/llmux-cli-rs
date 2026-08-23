import React, { useState, useEffect } from 'react';
import { BrowserRouter as Router, Routes, Route, Link, useLocation, Navigate, Outlet } from 'react-router-dom';
import Login from './routes/login';
import { useAuthStore } from './stores/auth';
import {
  LayoutDashboard,
  Users,
  Box,
  Settings,
  Info,
  ChevronRight,
  Zap,
  Key as KeyIcon,
  Menu,
  X,
  BarChart3,
  LogOut,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import Accounts from './routes/accounts';
import Models from './routes/models';
import Dashboard from './routes/dashboard';
import SettingsPage from './routes/settings';
import About from './routes/about';
import KeysPage from './routes/keys';
import StatsPage from './routes/stats';
import { useSettingsStore } from './stores/settings';
import { cn } from './lib/utils'
import { StatusDot } from './components/shared/StatusDot'
import { Button } from '@/components/ui/button'

const LanguageSwitcher = () => {
  const { i18n } = useTranslation();
  const currentLang = i18n.language.startsWith('zh') ? 'zh' : 'en';

  return (
    <div className="flex border border-border rounded-lg overflow-hidden">
      {['zh', 'en'].map(lang => (
        <Button
          key={lang}
          variant={currentLang === lang ? "default" : "ghost"}
          size="sm"
          onClick={() => i18n.changeLanguage(lang)}
          className={cn(
            "h-auto px-2 py-1 text-[10px] font-semibold rounded-none",
            currentLang === lang ? "" : "text-muted-foreground hover:bg-muted"
          )}
        >
          {lang.toUpperCase()}
        </Button>
      ))}
    </div>
  );
};

const NavItem = ({ to, icon: Icon, labelKey, onClick }: { to: string; icon: any; labelKey: string; onClick?: () => void }) => {
  const location = useLocation();
  const isActive = location.pathname === to;
  const { t } = useTranslation();

  return (
    <Link
      to={to}
      onClick={onClick}
      className={cn(
        "flex items-center gap-3 px-3 py-2 rounded-lg text-sm font-medium transition-colors",
        isActive
          ? "border-l-2 border-primary bg-primary/5 text-primary pl-2.5"
          : "text-muted-foreground hover:bg-muted"
      )}
    >
      <Icon size={18} />
      <span>{t(labelKey)}</span>
      {isActive && <ChevronRight size={14} className="ml-auto opacity-40" />}
    </Link>
  );
};

function ProtectedLayout() {
  const { isAuthenticated, checkAuth } = useAuthStore();
  useEffect(() => { if (isAuthenticated === null) checkAuth(); }, []); // eslint-disable-line
  if (isAuthenticated === null) return <div className="flex h-[100dvh] items-center justify-center text-sm text-muted-foreground">Loading...</div>;
  if (!isAuthenticated) return <Navigate to="/login" replace />;
  return <Outlet />;
}

const LogoutButton = () => {
  const { isAuthenticated, logout } = useAuthStore();
  const { t } = useTranslation();
  if (!isAuthenticated) return null;
  return (
    <Button variant="ghost" size="sm" onClick={logout} title={t('common.logout', '退出登录')}>
      <LogOut size={16} className="mr-1.5" />
      {t('common.logout', '退出')}
    </Button>
  );
};

function Shell() {
  const { t } = useTranslation();
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);
  const location = useLocation();

  useEffect(() => {
    setIsSidebarOpen(false);
  }, [location.pathname]);

  return (
    <div className="flex h-screen bg-background text-foreground overflow-hidden">
      {isSidebarOpen && (
        <div
          className="fixed inset-0 bg-background/80 backdrop-blur-sm z-40 lg:hidden"
          onClick={() => setIsSidebarOpen(false)}
        />
      )}

      <aside className={cn(
        "fixed inset-y-0 left-0 w-64 border-r border-border bg-card/80 backdrop-blur-xl flex flex-col z-50 transition-transform duration-300 ease-in-out lg:relative lg:translate-x-0",
        isSidebarOpen ? "translate-x-0" : "-translate-x-full"
      )}>
        <div className="px-6 py-8 flex items-center justify-between">
           <div className="flex items-center gap-3">
              <div className="bg-primary text-primary-foreground p-1.5 rounded-lg">
                 <Zap size={20} fill="currentColor" />
              </div>
              <h1 className="text-xl font-bold tracking-tight">LLMux</h1>
           </div>
           <Button
             variant="ghost"
             size="icon"
             onClick={() => setIsSidebarOpen(false)}
             className="lg:hidden"
           >
             <X size={20} />
           </Button>
        </div>

        <nav className="flex-1 px-3 space-y-1 overflow-y-auto">
          <div className="text-xs font-bold text-muted-foreground/50 uppercase tracking-wider px-3 mb-2">{t('common.menuCore')}</div>
          <NavItem to="/" icon={LayoutDashboard} labelKey="common.dashboard" />
          <NavItem to="/accounts" icon={Users} labelKey="common.accounts" />
          <NavItem to="/models" icon={Box} labelKey="common.models" />
          <NavItem to="/keys" icon={KeyIcon} labelKey="common.keys" />
          <NavItem to="/stats" icon={BarChart3} labelKey="common.usage" />

          <div className="pt-6 text-xs font-bold text-muted-foreground/50 uppercase tracking-wider px-3 mb-2">{t('common.menuPref')}</div>
          <NavItem to="/settings" icon={Settings} labelKey="common.settings" />
          <NavItem to="/about" icon={Info} labelKey="common.about" />
        </nav>

        <div className="p-4 border-t border-border mt-auto">
          <div className="flex items-center gap-2.5 text-xs text-muted-foreground">
            <StatusDot status="online" />
            {t('common.systemNormal')}
          </div>
        </div>
      </aside>

      <main className="flex-1 flex flex-col overflow-hidden w-full">
        <header className="h-14 border-b border-border/50 flex items-center px-4 lg:px-10 bg-card/50 backdrop-blur-md sticky top-0 z-30">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => setIsSidebarOpen(true)}
            className="lg:hidden -ml-2 mr-2"
          >
            <Menu size={20} />
          </Button>
          <div className="flex-1">
            <h2 className="text-sm font-bold lg:hidden">LLMux</h2>
          </div>
          <div className="flex items-center gap-4">
             <LogoutButton />
             <LanguageSwitcher />
          </div>
        </header>

        <div className="flex-1 overflow-y-auto">
          <div className="p-4 lg:p-10 max-w-[1600px] mx-auto w-full">
            <Outlet />
          </div>
        </div>
      </main>
    </div>
  );
}

function App() {
  const { config, fetchSettings } = useSettingsStore();

  useEffect(() => {
    fetchSettings();
  }, []);

  useEffect(() => {
    const theme = config.theme || 'dark';
    if (theme === 'light') {
      document.documentElement.classList.remove('dark');
    } else {
      document.documentElement.classList.add('dark');
    }
  }, [config.theme]);

  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route element={<ProtectedLayout />}>
        <Route element={<Shell />}>
          <Route path="/" element={<Dashboard />} />
          <Route path="/accounts" element={<Accounts />} />
          <Route path="/models" element={<Models />} />
          <Route path="/keys" element={<KeysPage />} />
          <Route path="/stats" element={<StatsPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="/about" element={<About />} />
        </Route>
      </Route>
    </Routes>
  );
}

export default function Root() {
  return (
    <Router basename={import.meta.env.BASE_URL}>
      <App />
    </Router>
  );
}
