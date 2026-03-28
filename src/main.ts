import { createApp } from 'vue'
import { createRouter, createWebHistory } from 'vue-router'
import App from './App.vue'
import ApiManager from './views/ApiManager.vue'
import ChatView from './views/ChatView.vue'
import AgentView from './views/AgentView.vue'
import PluginView from './views/PluginView.vue'
import './style.css'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/chat' },
    { path: '/chat', component: ChatView },
    { path: '/apis', component: ApiManager },
    { path: '/agent', component: AgentView },
    { path: '/plugins', component: PluginView },
  ]
})

const app = createApp(App)
app.use(router)
app.mount('#app')
