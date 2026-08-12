// src/router/index.tsx
import { createBrowserRouter } from "react-router-dom";
import { AppLayout } from "@/components/layout/AppLayout";
import { Dashboard } from "@/pages/Dashboard";
import { AccountList } from "@/pages/accounts/AccountList";
import { KeyList } from "@/pages/keys/KeyList";
import { ModelBrowser } from "@/pages/models/ModelBrowser";
import { ModelHealth } from "@/pages/models/ModelHealth";
import { VendorList } from "@/pages/vendors/VendorList";
import { SettingsLayout } from "@/pages/settings/SettingsLayout";
import { GeneralSettings } from "@/pages/settings/GeneralSettings";
import { CliSettings } from "@/pages/settings/CliSettings";
import { ImportExport } from "@/pages/settings/ImportExport";

export const router = createBrowserRouter([
  {
    element: <AppLayout />,
    children: [
      { path: "/", element: <Dashboard /> },
      { path: "/accounts", element: <AccountList /> },
      { path: "/keys", element: <KeyList /> },
      { path: "/models", element: <ModelBrowser /> },
      { path: "/models/health", element: <ModelHealth /> },
      { path: "/vendors", element: <VendorList /> },
      { path: "/setup", element: <CliSettings /> },
      {
        path: "/settings",
        element: <SettingsLayout />,
        children: [
          { index: true, element: <GeneralSettings /> },
          { path: "import-export", element: <ImportExport /> },
        ],
      },
    ],
  },
]);
