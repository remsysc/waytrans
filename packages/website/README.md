# waytrans Website

[![Astro](https://img.shields.io/badge/Astro-BC52EE?logo=astro&logoColor=fff)](#)
[![Tailwind CSS](https://img.shields.io/badge/Tailwind%20CSS-06B6D4?logo=tailwindcss&logoColor=fff)](#)
[![TypeScript](https://img.shields.io/badge/TypeScript-3178C6?logo=typescript&logoColor=fff)](#)
[![MIT License](https://img.shields.io/badge/License-MIT-green.svg)](#)

Modern, brutalist-styled landing page for waytrans built with Astro and Tailwind CSS.

## ✨ Features

- **⚡ Astro**: Lightning-fast static site generation
- **🎨 Tailwind CSS**: Utility-first CSS framework
- **🧩 Component-based**: Reusable Astro components
- **📱 Responsive**: Mobile-first design
- **🎯 Interactive**: Demo translation showcase
- **🔧 TypeScript**: Type-safe development
- **🎭 Brutalist Design**: Bold, functional aesthetics
- **📚 Documentation**: Full docs page with search and navigation
- **🔍 Search**: Real-time search functionality
- **📑 Sidebar Navigation**: Organized content sections

## 🚀 Quick Start

```bash
# Install dependencies
npm install

# Start development server
npm run dev

# Build for production
npm run build

# Preview production build
npm run preview
```

## 📁 Project Structure

```
src/
├── components/          # Reusable Astro components
│   ├── Button.astro    # Styled button component
│   ├── Card.astro      # Card container component
│   ├── CodeBlock.astro # Command line code blocks
│   ├── FeatureCard.astro # Feature showcase cards
│   ├── Header.astro    # Site navigation header
│   └── Hero.astro      # Hero section component
├── pages/
│   ├── index.astro     # Main landing page
│   └── docs.astro      # Documentation page with search
├── styles/
│   └── global.css      # Global styles and Tailwind config
└── env.d.ts           # TypeScript environment types
```

## 🎨 Design System

### Colors
- **Primary Blue**: `#4f8cff` - Interactive elements and accents
- **Background**: `#ffffff` - Clean white base
- **Text**: `#0b1020` - Dark readable text
- **Borders**: `#000000` - Bold black outlines

### Typography
- **Headings**: Space Grotesk (700 weight)
- **Body**: Space Grotesk (500 weight)  
- **Monospace**: Space Mono (400/700 weight)

### Components
- **Brutalist Shadows**: `4px 4px 0 black` for depth
- **Grid Background**: 40px grid pattern overlay
- **Bold Borders**: 2.5px black borders throughout

## 🧩 Components

### `<Button>`
Versatile button component with primary/secondary variants:
```astro
<Button variant="primary" href="/link">Primary Button</Button>
<Button variant="secondary">Secondary Button</Button>
```

### `<Card>`
Container component with brutalist styling:
```astro
<Card variant="primary">Content goes here</Card>
<Card variant="accent">Highlighted content</Card>
```

### `<FeatureCard>`
Feature showcase with consistent styling:
```astro
<FeatureCard title="Feature Name" variant="accent">
  Feature description content
</FeatureCard>
```

### `<CodeBlock>`
Command line code with copy functionality:
```astro
<CodeBlock code="npm install waytrans" copyable={true} />
```

## 🛠️ Development

### Prerequisites
- Node.js 18+ 
- npm or yarn package manager

### Local Development
1. Clone the repository
2. Navigate to `packages/website`
3. Run `npm install`
4. Run `npm run dev`
5. Open `http://localhost:4321`

### Building
```bash
npm run build
```
Outputs static files to `dist/` directory.

### Deployment
The site is built as static HTML/CSS/JS and can be deployed to:
- GitHub Pages
- Netlify
- Vercel
- Any static hosting service

## 📄 License

MIT License - see the [LICENSE](../../LICENSE) file for details.

---

**Part of the [waytrans](../../) project** - Instant in-place screen OCR translator for Wayland desktops.