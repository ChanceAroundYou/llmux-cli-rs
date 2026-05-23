import React from 'react';
import { useTranslation } from 'react-i18next';
import { 
  Heart, 
  Github, 
  Globe, 
  Zap,
  ShieldCheck,
  Cpu,
  Code2,
  Terminal,
  Box
} from 'lucide-react';

const FeatureItem = ({ icon: Icon, title, desc }: { icon: any, title: string, desc: string }) => (
  <div className="flex gap-4 p-5 rounded-xl border border-border bg-card">
     <div className="p-2 bg-primary/10 text-primary rounded-lg h-fit">
        <Icon size={20} />
     </div>
     <div className="space-y-1">
        <h4 className="text-sm font-bold">{title}</h4>
        <p className="text-[11px] text-muted-foreground leading-relaxed">{desc}</p>
     </div>
  </div>
);

export default function About() {
  const { t } = useTranslation();

  return (
    <div className="max-w-3xl mx-auto space-y-12 animate-in fade-in duration-500 py-6">
      {/* Hero */}
      <div className="space-y-4 text-center">
        <h1 className="text-4xl font-bold tracking-tight">LLMux Gateway</h1>
        <p className="text-base text-muted-foreground max-w-xl mx-auto">
           {t('about.desc')}
        </p>
      </div>

      {/* Buttons */}
      <div className="flex flex-col sm:flex-row items-center justify-center gap-3">
         <a 
           href="https://ifdian.net/a/llmux" 
           target="_blank" 
           rel="noopener noreferrer"
           className="flex items-center gap-2 px-6 py-2.5 bg-[#946ce6] text-white rounded-lg text-sm font-bold shadow-sm hover:opacity-90 transition-all"
         >
            <Heart size={16} fill="currentColor" />
            {t('about.sponsor')}
         </a>
         <a 
           href="https://github.com/zhMoody/llmux-cli" 
           target="_blank" 
           rel="noopener noreferrer"
           className="flex items-center gap-2 px-6 py-2.5 bg-card border border-border rounded-lg text-sm font-bold hover:bg-muted transition-all"
         >
           <Github size={16} />
           Star on GitHub
         </a>
      </div>

      {/* Feature Grid */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
         <FeatureItem 
           icon={Zap} 
           title={t('about.features.speed')} 
           desc={t('about.features.speedDesc')} 
         />
         <FeatureItem 
           icon={ShieldCheck} 
           title={t('about.features.privacy')} 
           desc={t('about.features.privacyDesc')} 
         />
         <FeatureItem 
           icon={Cpu} 
           title={t('about.features.agnostic')} 
           desc={t('about.features.agnosticDesc')} 
         />
         <FeatureItem 
           icon={Globe} 
           title={t('about.features.multi')} 
           desc={t('about.features.multiDesc')} 
         />
      </div>

      {/* Tech Stack */}
      <div className="space-y-4 border-t border-border pt-8">
         <div className="text-[10px] font-bold text-muted-foreground uppercase tracking-[0.2em]">{t('about.tech')}</div>
         <div className="flex flex-wrap gap-x-8 gap-y-4">
            {[
              { name: 'React', icon: Code2 },
              { name: 'TypeScript', icon: Box },
              { name: 'Bun', icon: Zap },
              { name: 'Vite', icon: Terminal }
            ].map(tech => (
              <div key={tech.name} className="flex items-center gap-2 text-xs font-semibold text-muted-foreground italic">
                <tech.icon size={14} />
                {tech.name}
              </div>
            ))}
         </div>
      </div>

      {/* Footer */}
      <div className="pt-8 text-[10px] text-muted-foreground font-medium opacity-50 flex justify-between items-center border-t border-border/40">
         <span>LLMux Engine v0.1.0-alpha · Open Source · AGPL-3.0</span>
         <span>© 2026</span>
      </div>
    </div>
  );
}
