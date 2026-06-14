package com.leanctx.plugin.psi

import com.intellij.psi.PsiClass
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.PsiMethod
import com.intellij.psi.PsiNameIdentifierOwner
import com.intellij.psi.PsiNamedElement
import com.intellij.psi.search.GlobalSearchScope
import com.intellij.psi.search.searches.ClassInheritorsSearch
import com.intellij.psi.search.searches.OverridingMethodsSearch
import com.intellij.util.Processor
import com.leanctx.plugin.dto.TypeHierarchyNodeDTO
import com.leanctx.plugin.dto.TypeHierarchyResponse
import com.leanctx.plugin.server.BackendException
import org.jetbrains.kotlin.asJava.toLightClass
import org.jetbrains.kotlin.asJava.toLightMethods
import org.jetbrains.kotlin.psi.KtClassOrObject
import org.jetbrains.kotlin.psi.KtNamedFunction

/**
 * Super/subtype tree for a class/interface or method. Language-neutral via Kotlin light
 * classes (KtClassOrObject.toLightClass()) → PsiClass/PsiMethod APIs work for Kotlin + Java.
 * Transitive with hard depth + node caps; must run inside a smart-mode ReadAction off the EDT.
 */
class TypeHierarchyResolver(private val locator: PsiLocator) {

    companion object {
        const val MAX_DEPTH = 5
        const val MAX_NODES = 200
    }

    private class Budget(var nodes: Int = 0, var truncated: Boolean = false)

    fun resolve(file: PsiFile, line: Int, character: Int, direction: String, scope: String): TypeHierarchyResponse {
        val target = resolveNamed(file, line, character)
        val searchScope = if (scope == "all") GlobalSearchScope.allScope(file.project)
        else GlobalSearchScope.projectScope(file.project)
        val wantSub = direction == "subtypes"
        val budget = Budget()

        val psiClass = asPsiClass(target)
        val root: TypeHierarchyNodeDTO = if (psiClass != null) {
            buildClassNode(psiClass, wantSub, searchScope, 0, budget)
        } else {
            val psiMethod = asPsiMethod(target)
                ?: throw BackendException("UNSUPPORTED_LANGUAGE", "type_hierarchy needs a class or method")
            buildMethodNode(psiMethod, wantSub, searchScope, 0, budget)
        }
        return TypeHierarchyResponse(root, budget.truncated)
    }

    private fun buildClassNode(cls: PsiClass, sub: Boolean, scope: GlobalSearchScope, depth: Int, b: Budget): TypeHierarchyNodeDTO {
        val children = ArrayList<TypeHierarchyNodeDTO>()
        if (depth < MAX_DEPTH) {
            val next: List<PsiClass> = if (sub) directSubclasses(cls, scope) else cls.supers.toList()
            for (n in next) {
                if (b.nodes >= MAX_NODES) { b.truncated = true; break }
                b.nodes++
                children.add(buildClassNode(n, sub, scope, depth + 1, b))
            }
        } else if ((if (sub) directSubclasses(cls, scope) else cls.supers.toList()).isNotEmpty()) {
            b.truncated = true
        }
        return nodeOf(cls, children)
    }

    private fun buildMethodNode(m: PsiMethod, sub: Boolean, scope: GlobalSearchScope, depth: Int, b: Budget): TypeHierarchyNodeDTO {
        val children = ArrayList<TypeHierarchyNodeDTO>()
        if (depth < MAX_DEPTH) {
            val next: List<PsiMethod> = if (sub) directOverriders(m, scope) else m.findSuperMethods().toList()
            for (n in next) {
                if (b.nodes >= MAX_NODES) { b.truncated = true; break }
                b.nodes++
                children.add(buildMethodNode(n, sub, scope, depth + 1, b))
            }
        } else if ((if (sub) directOverriders(m, scope) else m.findSuperMethods().toList()).isNotEmpty()) {
            b.truncated = true
        }
        return nodeOf(m, children)
    }

    private fun directSubclasses(cls: PsiClass, scope: GlobalSearchScope): List<PsiClass> {
        val out = ArrayList<PsiClass>()
        // checkDeep=false → direct inheritors only; recursion builds the tree.
        ClassInheritorsSearch.search(cls, scope, false).forEach(Processor { c: PsiClass -> out.add(c); true })
        return out
    }

    private fun directOverriders(m: PsiMethod, scope: GlobalSearchScope): List<PsiMethod> {
        val out = ArrayList<PsiMethod>()
        OverridingMethodsSearch.search(m, scope, false).forEach(Processor { mm: PsiMethod -> out.add(mm); true })
        return out
    }

    private fun nodeOf(element: PsiElement, children: List<TypeHierarchyNodeDTO>): TypeHierarchyNodeDTO {
        val nav = element.navigationElement ?: element
        val name = (element as? PsiNamedElement)?.name ?: "?"
        val loc = locator.toLocation(nav)
        val path = loc?.path ?: ""
        val line = (loc?.range?.start?.line ?: 0) + 1 // 0-based PSI → 1-based wire (constraint 4)
        return TypeHierarchyNodeDTO(name, path, line, children)
    }

    private fun asPsiClass(element: PsiElement): PsiClass? = when (element) {
        is PsiClass -> element
        is KtClassOrObject -> element.toLightClass()
        else -> null
    }

    private fun asPsiMethod(element: PsiElement): PsiMethod? = when (element) {
        is PsiMethod -> element
        is KtNamedFunction -> element.toLightMethods().firstOrNull()
        else -> null
    }

    private fun resolveNamed(file: PsiFile, line: Int, character: Int): PsiElement {
        val offset = locator.offsetOf(file, line, character)
        file.findReferenceAt(offset)?.resolve()?.let { return it }
        val element = file.findElementAt(offset)
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "no element at $line:$character")
        val decl = generateSequence(element) { it.parent }
            .firstOrNull { it is KtClassOrObject || it is KtNamedFunction || it is PsiClass || it is PsiMethod }
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "no class/method at $line:$character")
        // A bare declaration only counts when the caret lands on its name identifier — landing on
        // the `class`/`fun` keyword or whitespace is "no symbol at position" (navigation semantics).
        val nameId = (decl as? PsiNameIdentifierOwner)?.nameIdentifier
        if (nameId != null && !nameId.textRange.containsOffset(offset)) {
            throw BackendException("NO_SYMBOL_AT_POSITION", "no class/method name at $line:$character")
        }
        return decl
    }
}
